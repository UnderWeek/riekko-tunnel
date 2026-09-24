//! Full-system traffic capture via a TUN device, bridged to our local SOCKS5
//! listener by the official `tun2socks` project
//! (https://github.com/xjasonlyu/tun2socks). This is what actually routes
//! *all* traffic — not just apps that read system proxy settings — through
//! the tunnel, the same way real VPN clients work.
//!
//! Creating and configuring a TUN interface is a privileged OS operation on
//! every platform. Doing it the way Apple sanctions (NetworkExtension /
//! `NEPacketTunnelProvider`, what Shadowrocket uses) requires a paid Apple
//! Developer certificate and a signed, notarized app plus an installed
//! system-extension helper — none of which this project has. So, like every
//! other unsigned cross-platform client (NekoBox, etc.), this needs one
//! administrator/root prompt (osascript / pkexec / UAC) to bring the tunnel
//! up.
//!
//! That single elevated script also starts a small privileged *watchdog*
//! that lives exactly as long as the tunnel. It:
//! - tears everything down when the app drops a stop-signal file (an
//!   unprivileged write), so disconnecting needs no second prompt on any
//!   platform, and deletes the file to acknowledge;
//! - tears everything down on its own if the app process dies (crash,
//!   force quit), so a dead app can't leave the whole machine routed into a
//!   tunnel nothing is serving anymore;
//! - re-asserts routes the OS dropped (macOS does this on Wi-Fi changes and
//!   sleep/wake) and re-pins the server's own route when the default
//!   gateway changes;
//! - writes a one-word status file (`ok` / `degraded` / `dead` /
//!   `stopped`) every second, which is how the app checks tunnel health
//!   without spawning `route`/`powershell` itself on every tick.
//!
//! Privileged state (pid files, the watchdog script, tun2socks's log) lives
//! in a root-owned directory on macOS/Linux — never in world-writable /tmp,
//! where another local user could plant symlinks for root to write through.
//!
//! Config recipe (device name, routes, split-default trick) follows
//! tun2socks's own documented examples:
//! https://github.com/xjasonlyu/tun2socks/wiki/Examples

use crate::sys;
use futures_util::StreamExt;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

// NOT 198.18.0.1 — that's the address every tun2socks example (and,
// empirically, Shadowrocket) defaults to. Two TUN interfaces claiming the
// identical point-to-point peer address makes the kernel's route resolution
// ambiguous between them, so whichever fires up next can silently start
// losing the route lookup. Staying in the same reserved benchmarking block
// (RFC 2544, 198.18.0.0/15) but away from its very first address avoids
// colliding with tools that use the obvious default.
const TUN_IP: &str = "198.19.249.1";

/// A status word older than this isn't trusted as current.
const STATUS_FRESH_FOR: Duration = Duration::from_secs(5);
/// How long the status may stay stale or unreadable before the watchdog is
/// presumed dead. Measured on the app's own monotonic clock: file mtimes
/// follow the wall clock, which jumps across sleep/wake and NTP fixes, and
/// a watchdog that simply hasn't had its next turn yet (just woke up, slow
/// PowerShell start, mid-rename of the file) must not look like a crash.
const UNKNOWN_GRACE: Duration = Duration::from_secs(20);
/// How long a disconnect waits for the watchdog to acknowledge the stop
/// signal before falling back to an elevated teardown.
const STOP_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// What the tunnel needs to know to route around itself.
pub struct TunRequest<'a> {
    pub socks_port: u16,
    /// The VPN server's IPv4 address, routed around the tunnel so the
    /// core's own connection doesn't loop back into it. `None` for an
    /// IPv6 server: only IPv4 is captured, so nothing needs excluding.
    pub server_ip: Option<Ipv4Addr>,
    /// The xray/hysteria process, killed by the watchdog if the app dies.
    pub core_pid: u32,
    /// A private, per-user directory for the stop signal (and, on Windows,
    /// all of the tunnel's bookkeeping).
    pub work_dir: &'a Path,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TunStatus {
    /// Traffic is routed through the tunnel.
    Healthy,
    /// The tunnel is up, but the OS isn't routing through it right now
    /// (the watchdog is re-adding routes) — worth a "Reconnecting".
    Degraded,
    /// No fresh word from the watchdog yet; keep the last known state.
    Unknown,
    /// tun2socks or the watchdog is gone; the tunnel no longer exists.
    Gone,
}

/// A running tunnel. Dropping it tears the tunnel down.
pub struct TunHandle {
    work_dir: PathBuf,
    stop_file: PathBuf,
    status_file: PathBuf,
    up_bytes: Arc<AtomicU64>,
    down_bytes: Arc<AtomicU64>,
    watcher_stop: Arc<AtomicBool>,
    unknown_since: std::sync::Mutex<Option<Instant>>,
    closed: bool,
}

impl TunHandle {
    /// Total bytes sent / received since connect, as reported live by
    /// tun2socks. `(up, down)`.
    pub fn traffic_bytes(&self) -> (u64, u64) {
        (
            self.up_bytes.load(Ordering::Relaxed),
            self.down_bytes.load(Ordering::Relaxed),
        )
    }

    /// Cheap, unprivileged health check: reads the watchdog's status file.
    pub fn status(&self) -> TunStatus {
        let (word, fresh) = read_status(&self.status_file);
        let status = match word.as_deref() {
            // Final words: the watchdog tore everything down and exited.
            Some("dead" | "stopped") => TunStatus::Gone,
            Some("ok") if fresh => TunStatus::Healthy,
            Some("degraded") if fresh => TunStatus::Degraded,
            _ => TunStatus::Unknown,
        };
        let mut since = self.unknown_since.lock().unwrap();
        if status != TunStatus::Unknown {
            *since = None;
            return status;
        }
        if since.get_or_insert_with(Instant::now).elapsed() > UNKNOWN_GRACE {
            TunStatus::Gone
        } else {
            TunStatus::Unknown
        }
    }

    /// Stops trying to tear this tunnel down from the app — used at exit
    /// after a failed attempt, so `Drop` doesn't wait out the watchdog and
    /// prompt all over again. The watchdog still cleans up on its own once
    /// it sees the app process gone.
    pub fn disarm(&mut self) {
        self.closed = true;
        self.watcher_stop.store(true, Ordering::Relaxed);
    }

    /// Tears the tunnel down. Normally that's a stop signal the privileged
    /// watchdog acts on — no prompt. Only if the watchdog doesn't answer
    /// and something is demonstrably still left behind does this fall back
    /// to one more elevated teardown.
    ///
    /// On error the tunnel may still be up; the handle stays valid so the
    /// caller can keep the core running instead of leaving every app
    /// routed into a tunnel with nothing behind it.
    pub fn shutdown(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        let result = match self.stop_via_watchdog() {
            Ok(()) => Ok(()),
            Err(_) if !platform::needs_cleanup() => Ok(()),
            Err(_) => platform::elevated_teardown(&self.work_dir),
        };
        let _ = std::fs::remove_file(&self.stop_file);
        if result.is_ok() {
            self.closed = true;
            self.watcher_stop.store(true, Ordering::Relaxed);
        }
        result
    }

    fn stop_via_watchdog(&self) -> Result<(), String> {
        // After "dead"/"stopped" the watchdog has already torn down and
        // exited, so nobody would answer. Anything else (even a stale
        // status) gets the benefit of the doubt.
        if matches!(
            read_status(&self.status_file).0.as_deref(),
            Some("dead" | "stopped")
        ) {
            return Err("watchdog is not running".into());
        }
        std::fs::write(&self.stop_file, b"stop").map_err(|e| e.to_string())?;
        let deadline = Instant::now() + STOP_ACK_TIMEOUT;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            if !self.stop_file.exists() {
                return Ok(());
            }
        }
        Err("watchdog did not acknowledge the stop signal".into())
    }
}

impl Drop for TunHandle {
    fn drop(&mut self) {
        // Safety net: however this handle disappears — explicit disconnect,
        // the core dying, or the whole app quitting — the elevated
        // tun2socks process and the routes pointing at it must not outlive
        // it, or the user's internet stays broken after the app is gone.
        let _ = self.shutdown();
        self.watcher_stop.store(true, Ordering::Relaxed);
    }
}

/// The watchdog's last status word (if readable) and whether it was
/// written recently enough to describe the present.
fn read_status(path: &Path) -> (Option<String>, bool) {
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                <= STATUS_FRESH_FOR
        })
        .unwrap_or(false);
    let word = std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim_start_matches('\u{feff}').trim().to_string())
        .filter(|w| !w.is_empty());
    (word, fresh)
}

#[derive(serde::Deserialize)]
struct TrafficSample {
    up: u64,
    down: u64,
}

/// tun2socks emits one newline-delimited JSON object per second on
/// `/traffic` — the bytes moved in that second. This sums them into a
/// running total. It reconnects if the stream can't be opened yet (the REST
/// listener comes up a moment after the TUN device) or drops, until the
/// handle is shut down.
fn spawn_traffic_watcher(
    rest_port: u16,
    up_bytes: Arc<AtomicU64>,
    down_bytes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    tauri::async_runtime::spawn(async move {
        // Talking to 127.0.0.1: an HTTP(S)_PROXY from the environment
        // must not get in between.
        let Ok(client) = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .build()
        else {
            return;
        };
        let url = format!("http://127.0.0.1:{rest_port}/traffic");
        while !stop.load(Ordering::Relaxed) {
            if let Ok(response) = client.get(&url).send().await {
                let mut stream = response.bytes_stream();
                let mut buf: Vec<u8> = Vec::new();
                while let Some(Ok(chunk)) = stream.next().await {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        if let Ok(sample) = serde_json::from_slice::<TrafficSample>(&line) {
                            up_bytes.fetch_add(sample.up, Ordering::Relaxed);
                            down_bytes.fetch_add(sample.down, Ordering::Relaxed);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

fn sidecar_path(base: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "исполняемый файл без родительской директории".to_string())?;
    let name = if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    };
    let path = dir.join(name);
    if !path.exists() {
        return Err(format!(
            "{base} не найден рядом с приложением ({})",
            path.display()
        ));
    }
    Ok(path)
}

/// Brings the tunnel up (one elevation prompt) and starts its watchdog.
pub fn up(req: TunRequest) -> Result<TunHandle, String> {
    create_private_dir(req.work_dir)
        .map_err(|e| format!("Не удалось создать рабочую папку TUN: {e}"))?;
    let stop_file = req.work_dir.join("stop");
    let _ = std::fs::remove_file(&stop_file);
    let rest_port = crate::engine::free_port()?;
    let tun2socks = platform::tun2socks_path(req.work_dir)?;

    platform::up(&req, &tun2socks, rest_port, &stop_file)?;

    let up_bytes = Arc::new(AtomicU64::new(0));
    let down_bytes = Arc::new(AtomicU64::new(0));
    let watcher_stop = Arc::new(AtomicBool::new(false));
    spawn_traffic_watcher(
        rest_port,
        up_bytes.clone(),
        down_bytes.clone(),
        watcher_stop.clone(),
    );

    Ok(TunHandle {
        work_dir: req.work_dir.to_path_buf(),
        stop_file,
        status_file: platform::status_file(req.work_dir),
        up_bytes,
        down_bytes,
        watcher_stop,
        unknown_since: std::sync::Mutex::new(None),
        closed: false,
    })
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS + Linux: bash scripts run as root.
// ---------------------------------------------------------------------------

/// Shared by the "up" script, the watchdog and the fallback teardown.
/// Platform functions (`device_up`, `routes_up`, `routes_down`, `routes_ok`,
/// `heal`, `device_down`) are appended per OS.
#[cfg(unix)]
const UNIX_PRELUDE: &str = r#"set -u
RUN=@@RUN@@
STOP=@@STOP@@
T2S=@@T2S@@
DEV=@@DEV@@
TUN_IP=@@TUN_IP@@
SERVER_IP=@@SERVER_IP@@
SOCKS_PORT=@@SOCKS_PORT@@
REST_PORT=@@REST_PORT@@
APP_PID=@@APP_PID@@
CORE_PID=@@CORE_PID@@
DETACH=@@DETACH@@

alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }
cmd_of() { ps -p "$1" -o command= 2>/dev/null; }
is_t2s() { alive "${1:-}" && cmd_of "$1" | grep -q tun2socks; }
set_status() { printf '%s\n' "$1" > "$RUN/status.tmp" && chmod 644 "$RUN/status.tmp" && mv -f "$RUN/status.tmp" "$RUN/status"; }
teardown() {
  T=$(cat "$RUN/tun2socks.pid" 2>/dev/null || true)
  if is_t2s "$T"; then
    kill "$T" 2>/dev/null || true
    i=0
    while alive "$T" && [ "$i" -lt 30 ]; do sleep 0.1; i=$((i+1)); done
    if alive "$T"; then kill -9 "$T" 2>/dev/null || true; fi
  fi
  routes_down
  device_down
  rm -f "$RUN/tun2socks.pid" "$RUN/server_ip" "$RUN/server_gw"
}
"#;

#[cfg(unix)]
const UNIX_UP: &str = r#"
fail() { printf '%s\n' "$1" >&2; teardown; set_status dead; exit 1; }
mkdir -p "$RUN" || { echo "Cannot create $RUN" >&2; exit 1; }
chown 0 "$RUN" 2>/dev/null; chmod 755 "$RUN"
OLD_W=$(cat "$RUN/watchdog.pid" 2>/dev/null || true)
if alive "$OLD_W" && cmd_of "$OLD_W" | grep -q 'riekko-tunnel/watchdog'; then kill "$OLD_W" 2>/dev/null || true; fi
teardown
rm -f "$STOP" "$RUN/watchdog.pid"
if [ -n "$SERVER_IP" ]; then printf '%s\n' "$SERVER_IP" > "$RUN/server_ip"; fi
$DETACH "$T2S" --device "$DEV" --proxy "socks5://127.0.0.1:$SOCKS_PORT" --restapi "127.0.0.1:$REST_PORT" --loglevel warning </dev/null >"$RUN/tun2socks.log" 2>&1 &
echo $! > "$RUN/tun2socks.pid"
device_up || fail "TUN device did not come up: $(tail -c 400 "$RUN/tun2socks.log" 2>/dev/null)"
routes_up || fail "Could not install routes"
routes_ok || fail "Routes did not take effect"
cat > "$RUN/watchdog.sh" <<'RIEKKO_WATCHDOG_EOF'
@@WATCHDOG@@
RIEKKO_WATCHDOG_EOF
set_status ok
$DETACH nohup /bin/bash "$RUN/watchdog.sh" </dev/null >/dev/null 2>&1 &
echo $! > "$RUN/watchdog.pid"
exit 0
"#;

#[cfg(unix)]
const UNIX_WATCHDOG: &str = r#"
echo $$ > "$RUN/watchdog.pid"
finish() { teardown; set_status "$1"; rm -f "$RUN/watchdog.pid"; exit 0; }
while :; do
  if [ -e "$STOP" ]; then
    teardown; set_status stopped; rm -f "$STOP" "$RUN/watchdog.pid"; exit 0
  fi
  if ! alive "$APP_PID" || ! cmd_of "$APP_PID" | grep -qi riekko; then
    if alive "$CORE_PID" && cmd_of "$CORE_PID" | grep -Eq 'xray|hysteria'; then kill "$CORE_PID" 2>/dev/null; fi
    finish stopped
  fi
  if ! is_t2s "$(cat "$RUN/tun2socks.pid" 2>/dev/null || true)"; then finish dead; fi
  heal
  if routes_ok; then set_status ok; else set_status degraded; fi
  sleep 1
done
"#;

#[cfg(unix)]
const UNIX_TEARDOWN: &str = r#"
W=$(cat "$RUN/watchdog.pid" 2>/dev/null || true)
if alive "$W" && cmd_of "$W" | grep -q 'riekko-tunnel/watchdog'; then kill "$W" 2>/dev/null || true; fi
teardown
rm -f "$STOP" "$RUN/watchdog.pid"
set_status stopped 2>/dev/null
exit 0
"#;

#[cfg(unix)]
struct UnixScripts {
    prelude: String,
}

#[cfg(unix)]
impl UnixScripts {
    fn new(
        platform_functions: &str,
        run_dir: &str,
        stop_file: &Path,
        tun2socks: &Path,
        device: &str,
        detach: &str,
        req: Option<(&TunRequest, u16)>,
    ) -> Self {
        let (server_ip, socks_port, rest_port, core_pid) = match req {
            Some((r, rest)) => (
                r.server_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                r.socks_port,
                rest,
                r.core_pid,
            ),
            None => (String::new(), 0, 0, 0),
        };
        let vars = [
            ("RUN", sys::sh_quote(run_dir)),
            ("STOP", sys::sh_quote(&stop_file.to_string_lossy())),
            ("T2S", sys::sh_quote(&tun2socks.to_string_lossy())),
            ("DEV", sys::sh_quote(device)),
            ("TUN_IP", TUN_IP.to_string()),
            ("SERVER_IP", sys::sh_quote(&server_ip)),
            ("SOCKS_PORT", socks_port.to_string()),
            ("REST_PORT", rest_port.to_string()),
            ("APP_PID", std::process::id().to_string()),
            (
                "CORE_PID",
                if core_pid == 0 {
                    "''".into()
                } else {
                    core_pid.to_string()
                },
            ),
            ("DETACH", sys::sh_quote(detach)),
        ];
        let prelude = sys::render(&format!("{UNIX_PRELUDE}{platform_functions}"), &vars);
        Self { prelude }
    }

    fn up(&self) -> String {
        let watchdog = format!("#!/bin/bash\n{}{}", self.prelude, UNIX_WATCHDOG);
        let body = sys::render(UNIX_UP, &[("WATCHDOG", watchdog)]);
        format!("{}{}", self.prelude, body)
    }

    fn teardown(&self) -> String {
        format!("{}{}", self.prelude, UNIX_TEARDOWN)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    const DEVICE: &str = "utun123";
    const RUN_DIR: &str = "/var/run/riekko-tunnel";

    /// 0.0.0.0/8 is deliberately left out: `route add -net 0.0.0.0/1` gets
    /// parsed as the default route on macOS. The split still covers every
    /// routable address.
    const FUNCTIONS: &str = r#"
NETS="1.0.0.0/8 2.0.0.0/7 4.0.0.0/6 8.0.0.0/5 16.0.0.0/4 32.0.0.0/3 64.0.0.0/2 128.0.0.0/1 198.18.0.0/15"
default_gw() { route -n get default 2>/dev/null | awk '/gateway:/{print $2; exit}'; }
default_if() { route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}'; }
add_server_route() {
  [ -n "$SERVER_IP" ] || return 0
  GW=$(default_gw); IF=$(default_if)
  route -q -n delete -host "$SERVER_IP" >/dev/null 2>&1
  if [ -n "$GW" ]; then
    route -q -n add -host "$SERVER_IP" "$GW" >/dev/null 2>&1 || return 1
  elif [ -n "$IF" ]; then
    route -q -n add -host "$SERVER_IP" -interface "$IF" >/dev/null 2>&1 || return 1
  else
    return 1
  fi
  printf '%s\n' "$GW" > "$RUN/server_gw"
}
add_nets() { for n in $NETS; do route -q -n add -net "$n" "$TUN_IP" >/dev/null 2>&1; done; return 0; }
device_up() {
  T=$(cat "$RUN/tun2socks.pid" 2>/dev/null || true)
  i=0
  while ! ifconfig "$DEV" >/dev/null 2>&1; do
    alive "$T" || return 1
    i=$((i+1)); [ "$i" -gt 50 ] && return 1
    sleep 0.1
  done
  ifconfig "$DEV" "$TUN_IP" "$TUN_IP" up
}
device_down() { :; }
routes_up() { add_server_route || return 1; add_nets; }
routes_down() {
  for n in $NETS; do route -q -n delete -net "$n" "$TUN_IP" >/dev/null 2>&1; done
  OLD=$(cat "$RUN/server_ip" 2>/dev/null || true)
  if [ -n "$OLD" ]; then route -q -n delete -host "$OLD" >/dev/null 2>&1; fi
  return 0
}
routes_ok() { [ "$(route -n get 8.8.8.8 2>/dev/null | awk '/interface:/{print $2; exit}')" = "$DEV" ]; }
heal() {
  if [ -n "$SERVER_IP" ]; then
    SIF=$(route -n get "$SERVER_IP" 2>/dev/null | awk '/interface:/{print $2; exit}')
    GW=$(default_gw)
    OLD_GW=$(cat "$RUN/server_gw" 2>/dev/null || true)
    if [ "$SIF" = "$DEV" ] || { [ -n "$GW" ] && [ "$GW" != "$OLD_GW" ]; }; then add_server_route; fi
  fi
  routes_ok || add_nets
}
"#;

    pub fn status_file(_work_dir: &Path) -> PathBuf {
        Path::new(RUN_DIR).join("status")
    }

    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        sidecar_path("tun2socks")
    }

    fn scripts(stop_file: &Path, tun2socks: &Path, req: Option<(&TunRequest, u16)>) -> UnixScripts {
        UnixScripts::new(FUNCTIONS, RUN_DIR, stop_file, tun2socks, DEVICE, "", req)
    }

    /// Runs `script` as root behind the standard macOS admin prompt. The
    /// script travels as an argv item, so no AppleScript/shell escaping of
    /// its contents is needed.
    fn run_elevated(script: &str) -> Result<(), String> {
        let output = sys::command("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "do shell script \"/bin/bash -c \" & quoted form of (item 1 of argv) with prompt (item 2 of argv) with administrator privileges",
                "-e",
                "end run",
                script,
                "Riekko Tunnel настраивает TUN-интерфейс, чтобы весь трафик шёл через туннель.",
            ])
            .output()
            .map_err(|e| format!("osascript: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(
            if stderr.contains("-128") || stderr.contains("User canceled") {
                "Запрос пароля администратора отменён".to_string()
            } else {
                format!("Настройка TUN не удалась: {}", stderr.trim())
            },
        )
    }

    pub fn up(
        req: &TunRequest,
        tun2socks: &Path,
        rest_port: u16,
        stop_file: &Path,
    ) -> Result<(), String> {
        run_elevated(&scripts(stop_file, tun2socks, Some((req, rest_port))).up())
    }

    pub fn elevated_teardown(work_dir: &Path) -> Result<(), String> {
        let t2s = PathBuf::from("tun2socks");
        run_elevated(&scripts(&work_dir.join("stop"), &t2s, None).teardown())
    }

    /// Whether anything of ours is still in the routing table or the
    /// device list — i.e. whether a fallback teardown is worth a prompt.
    pub fn needs_cleanup() -> bool {
        let device_exists = sys::command("ifconfig")
            .arg(DEVICE)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let routed = sys::command("route")
            .args(["-n", "get", "8.8.8.8"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(DEVICE))
            .unwrap_or(false);
        device_exists || routed
    }

    #[cfg(test)]
    pub fn test_scripts() -> (String, String) {
        let req = TunRequest {
            socks_port: 10808,
            server_ip: Some(Ipv4Addr::new(203, 0, 113, 7)),
            core_pid: 4242,
            work_dir: Path::new("/tmp/riekko test's dir"),
        };
        let s = scripts(
            Path::new("/tmp/riekko test's dir/stop"),
            Path::new("/Applications/Riekko Tunnel.app/Contents/MacOS/tun2socks"),
            Some((&req, 9797)),
        );
        (s.up(), s.teardown())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    /// Same name the old builds used, so a persistent device they leaked
    /// gets cleaned up by `device_down` too.
    const DEVICE: &str = "utun123";
    const RUN_DIR: &str = "/run/riekko-tunnel";

    const FUNCTIONS: &str = r#"
via_of() { printf '%s\n' "$1" | sed -n 's/.* via \([^ ]*\).*/\1/p'; }
dev_of() { printf '%s\n' "$1" | sed -n 's/.* dev \([^ ]*\).*/\1/p'; }
pin_server() {
  # $1: a route line ("... via GW dev IF ..." or "... dev IF ...") to copy.
  # A server on this very machine never enters the tunnel, and a /32 in
  # "main" would override its local delivery (and its source address).
  case "$1" in local\ *) return 0 ;; esac
  VIA=$(via_of "$1"); ODEV=$(dev_of "$1")
  [ -n "$ODEV" ] && [ "$ODEV" != "$DEV" ] || return 1
  if [ -n "$VIA" ]; then
    ip route replace "$SERVER_IP/32" via "$VIA" dev "$ODEV"
  else
    ip route replace "$SERVER_IP/32" dev "$ODEV"
  fi
}
device_up() {
  T=$(cat "$RUN/tun2socks.pid" 2>/dev/null || true)
  i=0
  while [ ! -e "/sys/class/net/$DEV" ]; do
    alive "$T" || return 1
    i=$((i+1)); [ "$i" -gt 50 ] && return 1
    sleep 0.1
  done
  ip addr replace "$TUN_IP/15" dev "$DEV" && ip link set dev "$DEV" up
}
device_down() { ip link del "$DEV" >/dev/null 2>&1; return 0; }
routes_up() {
  if [ -n "$SERVER_IP" ]; then
    # Must be computed before the split routes exist, or it'd point at us.
    pin_server "$(ip -4 route get "$SERVER_IP" 2>/dev/null | head -n1)" || return 1
  fi
  ip route replace 0.0.0.0/1 dev "$DEV" && ip route replace 128.0.0.0/1 dev "$DEV"
}
routes_down() {
  ip route del 0.0.0.0/1 dev "$DEV" >/dev/null 2>&1
  ip route del 128.0.0.0/1 dev "$DEV" >/dev/null 2>&1
  OLD=$(cat "$RUN/server_ip" 2>/dev/null || true)
  if [ -n "$OLD" ]; then ip route del "$OLD/32" >/dev/null 2>&1; fi
  return 0
}
routes_ok() { ip -4 route get 8.8.8.8 2>/dev/null | grep -q "dev $DEV "; }
heal() {
  if [ -n "$SERVER_IP" ]; then
    S=$(ip -4 route show "$SERVER_IP/32" 2>/dev/null | head -n1)
    if [ -z "$S" ] || printf '%s' "$S" | grep -q linkdown; then
      pin_server "$(ip -4 route show default 2>/dev/null | grep -v linkdown | head -n1)" >/dev/null 2>&1
    fi
  fi
  if ! ip -4 route show 0.0.0.0/1 2>/dev/null | grep -q "dev $DEV"; then ip route replace 0.0.0.0/1 dev "$DEV" >/dev/null 2>&1; fi
  if ! ip -4 route show 128.0.0.0/1 2>/dev/null | grep -q "dev $DEV"; then ip route replace 128.0.0.0/1 dev "$DEV" >/dev/null 2>&1; fi
  return 0
}
"#;

    pub fn status_file(_work_dir: &Path) -> PathBuf {
        Path::new(RUN_DIR).join("status")
    }

    /// Inside an AppImage the bundled binaries live on a FUSE mount that
    /// root can't read, so root must be handed a copy outside it.
    pub fn tun2socks_path(work_dir: &Path) -> Result<PathBuf, String> {
        let bundled = sidecar_path("tun2socks")?;
        if std::env::var_os("APPIMAGE").is_none() {
            return Ok(bundled);
        }
        let copy = work_dir.join("tun2socks");
        let _ = std::fs::remove_file(&copy);
        std::fs::copy(&bundled, &copy)
            .map_err(|e| format!("Не удалось скопировать tun2socks: {e}"))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Не удалось скопировать tun2socks: {e}"))?;
        Ok(copy)
    }

    fn scripts(stop_file: &Path, tun2socks: &Path, req: Option<(&TunRequest, u16)>) -> UnixScripts {
        // setsid: a Ctrl-C in the terminal the app was started from must
        // not reach root's tun2socks/watchdog and strand the routes.
        UnixScripts::new(
            FUNCTIONS, RUN_DIR, stop_file, tun2socks, DEVICE, "setsid", req,
        )
    }

    /// Runs `script` as root behind the desktop's PolicyKit prompt.
    fn run_elevated(script: &str) -> Result<(), String> {
        let output = sys::command("pkexec")
            .args(["/bin/bash", "-c", script])
            .output()
            .map_err(|e| format!("pkexec: {e} (нужен PolicyKit)"))?;
        if output.status.success() {
            return Ok(());
        }
        // pkexec: 126 = dialog dismissed / not authorized, 127 = auth failed.
        Err(match output.status.code() {
            Some(126) => "Запрос пароля администратора отменён".to_string(),
            Some(127) => format!(
                "Не удалось получить права администратора (нет агента PolicyKit или в доступе отказано){}",
                match String::from_utf8_lossy(&output.stderr).trim() {
                    "" => String::new(),
                    detail => format!(": {detail}"),
                }
            ),
            _ => format!(
                "Настройка TUN не удалась: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }

    pub fn up(
        req: &TunRequest,
        tun2socks: &Path,
        rest_port: u16,
        stop_file: &Path,
    ) -> Result<(), String> {
        run_elevated(&scripts(stop_file, tun2socks, Some((req, rest_port))).up())
    }

    pub fn elevated_teardown(work_dir: &Path) -> Result<(), String> {
        let t2s = PathBuf::from("tun2socks");
        run_elevated(&scripts(&work_dir.join("stop"), &t2s, None).teardown())
    }

    pub fn needs_cleanup() -> bool {
        Path::new("/sys/class/net").join(DEVICE).exists()
    }

    #[cfg(test)]
    pub fn test_scripts() -> (String, String) {
        let req = TunRequest {
            socks_port: 10808,
            server_ip: Some(Ipv4Addr::new(203, 0, 113, 7)),
            core_pid: 4242,
            work_dir: Path::new("/tmp/riekko test's dir"),
        };
        let s = scripts(
            Path::new("/tmp/riekko test's dir/stop"),
            Path::new("/opt/Riekko Tunnel/tun2socks"),
            Some((&req, 9797)),
        );
        (s.up(), s.teardown())
    }
}

// ---------------------------------------------------------------------------
// Windows: PowerShell scripts run elevated via UAC.
// ---------------------------------------------------------------------------

/// Shared by the "up" script, the watchdog and the fallback teardown.
#[cfg(any(windows, test))]
const WIN_PRELUDE: &str = r#"$Dir = @@DIR@@
$T2S = @@T2S@@
$Stop = Join-Path $Dir 'stop'
$Dev = 'RiekkoTun'
$TunIp = '@@TUN_IP@@'
$ServerIp = @@SERVER_IP@@
$Socks = @@SOCKS_PORT@@
$Rest = @@REST_PORT@@
$AppPid = @@APP_PID@@
$CorePid = @@CORE_PID@@

function Set-Status([string]$s) {
  try {
    [IO.File]::WriteAllText((Join-Path $Dir 'status.tmp'), $s)
    Move-Item -LiteralPath (Join-Path $Dir 'status.tmp') -Destination (Join-Path $Dir 'status') -Force
  } catch {}
}

function Get-MainRoute {
  Get-NetRoute -DestinationPrefix '0.0.0.0/0' -AddressFamily IPv4 -PolicyStore ActiveStore -ErrorAction SilentlyContinue |
    Where-Object {
      $_.InterfaceAlias -ne $Dev -and
      (Get-NetIPInterface -InterfaceIndex $_.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue).ConnectionState -eq 'Connected'
    } |
    Sort-Object -Property @{ Expression = { [int]$_.RouteMetric + [int]$_.InterfaceMetric } } |
    Select-Object -First 1
}

function Add-ServerRoute($main) {
  Remove-NetRoute -DestinationPrefix "$ServerIp/32" -PolicyStore ActiveStore -Confirm:$false -ErrorAction SilentlyContinue
  New-NetRoute -DestinationPrefix "$ServerIp/32" -InterfaceIndex $main.ifIndex -NextHop $main.NextHop -RouteMetric 1 -PolicyStore ActiveStore -ErrorAction Stop | Out-Null
  [IO.File]::WriteAllText((Join-Path $Dir 'server_route'), "$($main.ifIndex)|$($main.NextHop)")
}

function Teardown {
  $ErrorActionPreference = 'SilentlyContinue'
  $t = Get-Content -LiteralPath (Join-Path $Dir 'tun2socks.pid') | Select-Object -First 1
  if ($t) {
    Get-Process -Id ([int]$t) | Where-Object { $_.ProcessName -like 'tun2socks*' } | Stop-Process -Force
  }
  $old = Get-Content -LiteralPath (Join-Path $Dir 'server_ip') | Select-Object -First 1
  if ($old) { Remove-NetRoute -DestinationPrefix "$old/32" -PolicyStore ActiveStore -Confirm:$false }
  foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1')) {
    netsh interface ipv4 delete route $prefix $Dev store=active | Out-Null
  }
  Remove-Item -LiteralPath (Join-Path $Dir 'tun2socks.pid'), (Join-Path $Dir 'server_ip'), (Join-Path $Dir 'server_route') -Force
}
"#;

#[cfg(any(windows, test))]
const WIN_UP: &str = r#"
$ErrorActionPreference = 'Stop'
function Fail([string]$msg) {
  [IO.File]::WriteAllText((Join-Path $Dir 'error.txt'), $msg)
  Teardown
  Set-Status 'dead'
  exit 1
}
try {
  $oldW = Get-Content -LiteralPath (Join-Path $Dir 'watchdog.pid') -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($oldW) {
    Get-Process -Id ([int]$oldW) -ErrorAction SilentlyContinue |
      Where-Object { $_.ProcessName -eq 'powershell' } | Stop-Process -Force -ErrorAction SilentlyContinue
  }
  Teardown
  Remove-Item -LiteralPath $Stop, (Join-Path $Dir 'error.txt'), (Join-Path $Dir 'watchdog.pid') -Force -ErrorAction SilentlyContinue

  $main = $null
  if ($ServerIp) {
    $main = Get-MainRoute
    if (-not $main) { Fail 'No active default route to reach the server through' }
    [IO.File]::WriteAllText((Join-Path $Dir 'server_ip'), $ServerIp)
  }

  $p = Start-Process -FilePath $T2S -WindowStyle Hidden -PassThru -ArgumentList @(
    '--device', $Dev, '--proxy', "socks5://127.0.0.1:$Socks",
    '--restapi', "127.0.0.1:$Rest", '--loglevel', 'warning')
  [IO.File]::WriteAllText((Join-Path $Dir 'tun2socks.pid'), [string]$p.Id)

  $ready = $false
  for ($i = 0; $i -lt 100; $i++) {
    if ($p.HasExited) { Fail "tun2socks exited right after start (code $($p.ExitCode))" }
    if (Get-NetAdapter -Name $Dev -ErrorAction SilentlyContinue) { $ready = $true; break }
    Start-Sleep -Milliseconds 100
  }
  if (-not $ready) { Fail 'The TUN adapter did not appear' }

  netsh interface ipv4 set address "name=$Dev" source=static "addr=$TunIp" mask=255.255.255.0 store=active | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail 'netsh could not assign the TUN address' }
  if ($ServerIp) { Add-ServerRoute $main }
  foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1')) {
    netsh interface ipv4 add route $prefix $Dev $TunIp metric=1 store=active | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "netsh could not add route $prefix" }
  }

  Set-Status 'ok'
  $watchdog = '"' + (Join-Path $Dir 'watchdog.ps1') + '"'
  Start-Process -FilePath 'powershell.exe' -WindowStyle Hidden -ArgumentList @(
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden', '-File', $watchdog)
  exit 0
} catch {
  Fail $_.Exception.Message
}
"#;

#[cfg(any(windows, test))]
const WIN_WATCHDOG: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[IO.File]::WriteAllText((Join-Path $Dir 'watchdog.pid'), [string]$PID)
function Heal-ServerRoute {
  $main = Get-MainRoute
  if (-not $main) { return }
  $have = Get-Content -LiteralPath (Join-Path $Dir 'server_route') | Select-Object -First 1
  if ("$($main.ifIndex)|$($main.NextHop)" -ne $have) { try { Add-ServerRoute $main } catch {} }
}
$tick = 0
while ($true) {
  if (Test-Path -LiteralPath $Stop) {
    Teardown; Set-Status 'stopped'
    Remove-Item -LiteralPath $Stop, (Join-Path $Dir 'watchdog.pid') -Force
    exit 0
  }
  if (-not (Get-Process -Id $AppPid)) {
    if ($CorePid) {
      Get-Process -Id $CorePid | Where-Object { $_.ProcessName -match '^(xray|hysteria)' } | Stop-Process -Force
    }
    Teardown; Set-Status 'stopped'
    Remove-Item -LiteralPath (Join-Path $Dir 'watchdog.pid') -Force
    exit 0
  }
  $t = Get-Content -LiteralPath (Join-Path $Dir 'tun2socks.pid') | Select-Object -First 1
  if (-not $t -or -not (Get-Process -Id ([int]$t))) {
    Teardown; Set-Status 'dead'
    Remove-Item -LiteralPath (Join-Path $Dir 'watchdog.pid') -Force
    exit 0
  }
  if ($ServerIp -and ($tick % 3) -eq 0) { Heal-ServerRoute }
  $routes = @(Get-NetRoute -DestinationPrefix '0.0.0.0/1' -PolicyStore ActiveStore | Where-Object { $_.InterfaceAlias -eq $Dev })
  if ($routes.Count -gt 0) { Set-Status 'ok' } else { Set-Status 'degraded' }
  $tick++
  Start-Sleep -Seconds 1
}
"#;

#[cfg(any(windows, test))]
const WIN_TEARDOWN: &str = r#"
$w = Get-Content -LiteralPath (Join-Path $Dir 'watchdog.pid') -ErrorAction SilentlyContinue | Select-Object -First 1
if ($w) {
  Get-Process -Id ([int]$w) -ErrorAction SilentlyContinue |
    Where-Object { $_.ProcessName -eq 'powershell' } | Stop-Process -Force -ErrorAction SilentlyContinue
}
Teardown
Remove-Item -LiteralPath $Stop, (Join-Path $Dir 'watchdog.pid') -Force -ErrorAction SilentlyContinue
Set-Status 'stopped'
exit 0
"#;

/// Renders the Windows scripts: `(up, watchdog, teardown)`.
#[cfg(any(windows, test))]
fn windows_scripts(
    work_dir: &Path,
    tun2socks: &Path,
    req: Option<(&TunRequest, u16)>,
) -> (String, String, String) {
    let (server_ip, socks_port, rest_port, core_pid) = match req {
        Some((r, rest)) => (
            r.server_ip.map(|ip| ip.to_string()).unwrap_or_default(),
            r.socks_port,
            rest,
            r.core_pid,
        ),
        None => (String::new(), 0, 0, 0),
    };
    let vars = [
        ("DIR", sys::ps_quote(&work_dir.to_string_lossy())),
        ("T2S", sys::ps_quote(&tun2socks.to_string_lossy())),
        ("TUN_IP", TUN_IP.to_string()),
        ("SERVER_IP", sys::ps_quote(&server_ip)),
        ("SOCKS_PORT", socks_port.to_string()),
        ("REST_PORT", rest_port.to_string()),
        ("APP_PID", std::process::id().to_string()),
        ("CORE_PID", core_pid.to_string()),
    ];
    let prelude = sys::render(WIN_PRELUDE, &vars);
    (
        format!("{prelude}{WIN_UP}"),
        format!("{prelude}{WIN_WATCHDOG}"),
        format!("{prelude}{WIN_TEARDOWN}"),
    )
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    const ADAPTER: &str = "RiekkoTun";
    /// ERROR_CANCELLED — the runner's exit code when UAC is declined.
    const UAC_CANCELLED: i32 = 1223;

    pub fn status_file(work_dir: &Path) -> PathBuf {
        work_dir.join("status")
    }

    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        sidecar_path("tun2socks")
    }

    /// Windows PowerShell 5.1 reads BOM-less scripts in the ANSI code page,
    /// which would mangle any non-ASCII path (a Cyrillic user name, say).
    fn write_script(path: &Path, script: &str) -> Result<(), String> {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(script.replace('\n', "\r\n").as_bytes());
        std::fs::write(path, bytes).map_err(|e| format!("Не удалось записать скрипт: {e}"))
    }

    /// Runs a script file elevated via UAC and waits for it. The runner is
    /// passed as `-EncodedCommand`, and the script path is quoted for
    /// `Start-Process`, which joins its argument list with bare spaces.
    fn run_elevated(script_path: &Path) -> Result<(), String> {
        let quoted_path = sys::ps_quote(&format!("\"{}\"", script_path.display()));
        let runner = format!(
            "try {{\n\
               $p = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WindowStyle Hidden -PassThru -ErrorAction Stop \
                 -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',{quoted_path})\n\
               $null = $p.Handle\n\
               $p.WaitForExit()\n\
               exit $p.ExitCode\n\
             }} catch {{ exit {UAC_CANCELLED} }}\n"
        );
        let status = sys::command("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &sys::ps_encode(&runner),
            ])
            .status()
            .map_err(|e| format!("powershell: {e}"))?;
        match status.code() {
            Some(0) => Ok(()),
            Some(UAC_CANCELLED) => Err("Запрос UAC отклонён".to_string()),
            _ => Err("сценарий завершился с ошибкой".to_string()),
        }
    }

    pub fn up(
        req: &TunRequest,
        tun2socks: &Path,
        rest_port: u16,
        _stop_file: &Path,
    ) -> Result<(), String> {
        let (up, watchdog, _) = windows_scripts(req.work_dir, tun2socks, Some((req, rest_port)));
        let up_path = req.work_dir.join("up.ps1");
        let error_path = req.work_dir.join("error.txt");
        let status_path = status_file(req.work_dir);
        let _ = std::fs::remove_file(&error_path);
        let _ = std::fs::remove_file(&status_path);
        write_script(&up_path, &up)?;
        write_script(&req.work_dir.join("watchdog.ps1"), &watchdog)?;

        let result = run_elevated(&up_path);
        let _ = std::fs::remove_file(&up_path);
        // The status file is the source of truth, not the exit code (which
        // `Start-Process -Verb RunAs` doesn't always relay): the elevated
        // script only writes "ok" once the adapter and routes are in place,
        // and from then on the watchdog owns the tunnel.
        if matches!(read_status(&status_path), (Some(ref w), true) if w == "ok" || w == "degraded")
        {
            return Ok(());
        }
        match result {
            Err(e) if e.contains("UAC") => Err(e),
            _ => {
                let detail = std::fs::read_to_string(&error_path)
                    .map(|s| s.trim_start_matches('\u{feff}').trim().to_string())
                    .unwrap_or_default();
                Err(format!(
                    "Настройка TUN не удалась: {}",
                    if detail.is_empty() {
                        "неизвестная ошибка".to_string()
                    } else {
                        detail
                    }
                ))
            }
        }
    }

    pub fn elevated_teardown(work_dir: &Path) -> Result<(), String> {
        let (_, _, teardown) = windows_scripts(work_dir, Path::new("tun2socks.exe"), None);
        let path = work_dir.join("down.ps1");
        write_script(&path, &teardown)?;
        let result = run_elevated(&path);
        let _ = std::fs::remove_file(&path);
        result
    }

    pub fn needs_cleanup() -> bool {
        sys::command("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("if (Get-NetAdapter -Name '{ADAPTER}' -ErrorAction SilentlyContinue) {{ exit 1 }} else {{ exit 0 }}"),
            ])
            .status()
            .map(|s| s.code() == Some(1))
            .unwrap_or(false)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;

    pub fn status_file(work_dir: &Path) -> PathBuf {
        work_dir.join("status")
    }
    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        Err("TUN-режим не поддерживается на этой платформе".to_string())
    }
    pub fn up(_req: &TunRequest, _t2s: &Path, _rest: u16, _stop: &Path) -> Result<(), String> {
        Err("TUN-режим не поддерживается на этой платформе".to_string())
    }
    pub fn elevated_teardown(_work_dir: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn needs_cleanup() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_with_status(path: &Path) -> TunHandle {
        TunHandle {
            work_dir: std::env::temp_dir(),
            stop_file: path.with_extension("stop"),
            status_file: path.to_path_buf(),
            up_bytes: Arc::new(AtomicU64::new(0)),
            down_bytes: Arc::new(AtomicU64::new(0)),
            watcher_stop: Arc::new(AtomicBool::new(false)),
            unknown_since: std::sync::Mutex::new(None),
            closed: true, // never run a real teardown from a test
        }
    }

    fn age(path: &Path, secs: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn status_reading_distinguishes_unknown_from_gone() {
        let path = std::env::temp_dir().join(crate::state::unique_id("riekko-test-status"));
        let handle = handle_with_status(&path);
        // Missing or stale is "unknown" (e.g. just woke from sleep), not dead.
        assert_eq!(handle.status(), TunStatus::Unknown);
        std::fs::write(&path, "ok\n").unwrap();
        assert_eq!(handle.status(), TunStatus::Healthy);
        std::fs::write(&path, "degraded").unwrap();
        assert_eq!(handle.status(), TunStatus::Degraded);
        std::fs::write(&path, "ok").unwrap();
        age(&path, 60);
        assert_eq!(handle.status(), TunStatus::Unknown);
        // ...until it has stayed unknown for the whole grace period.
        *handle.unknown_since.lock().unwrap() =
            Some(Instant::now() - UNKNOWN_GRACE - Duration::from_secs(1));
        assert_eq!(handle.status(), TunStatus::Gone);
        // A fresh word resets the grace clock.
        std::fs::write(&path, "ok").unwrap();
        assert_eq!(handle.status(), TunStatus::Healthy);
        assert!(handle.unknown_since.lock().unwrap().is_none());
        // Final words mean gone, however old.
        std::fs::write(&path, "stopped").unwrap();
        age(&path, 600);
        assert_eq!(handle.status(), TunStatus::Gone);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn windows_scripts_have_no_unfilled_placeholders() {
        let req = TunRequest {
            socks_port: 10808,
            server_ip: Some(Ipv4Addr::new(203, 0, 113, 7)),
            core_pid: 4242,
            work_dir: Path::new(r"C:\Users\O'Brien\AppData\Local\com.riekko.tunnel\tun"),
        };
        let (up, watchdog, teardown) = windows_scripts(
            req.work_dir,
            Path::new(r"C:\Program Files\Riekko Tunnel\tun2socks.exe"),
            Some((&req, 9797)),
        );
        for script in [&up, &watchdog, &teardown] {
            assert!(!script.contains("@@"));
            assert!(
                script.contains(r"$Dir = 'C:\Users\O''Brien\AppData\Local\com.riekko.tunnel\tun'")
            );
        }
        assert!(up.contains("$ServerIp = '203.0.113.7'"));
        assert!(watchdog.contains("$CorePid = 4242"));
    }

    /// Writes the rendered scripts out so CI (and a curious human) can run
    /// `bash -n` / shellcheck / PowerShell's parser over the real thing.
    #[test]
    fn dump_rendered_scripts_for_linting() {
        let Some(dir) = std::env::var_os("RIEKKO_DUMP_SCRIPTS") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (up, teardown) = platform::test_scripts();
            std::fs::write(dir.join("up.sh"), &up).unwrap();
            std::fs::write(dir.join("teardown.sh"), &teardown).unwrap();
            let marker = "<<'RIEKKO_WATCHDOG_EOF'\n";
            let start = up.find(marker).unwrap() + marker.len();
            let end = up[start..].find("\nRIEKKO_WATCHDOG_EOF").unwrap() + start;
            std::fs::write(dir.join("watchdog.sh"), &up[start..end]).unwrap();
        }
        let req = TunRequest {
            socks_port: 10808,
            server_ip: Some(Ipv4Addr::new(203, 0, 113, 7)),
            core_pid: 4242,
            work_dir: Path::new(r"C:\Users\O'Brien\AppData\Local\com.riekko.tunnel\tun"),
        };
        let (up, watchdog, teardown) = windows_scripts(
            req.work_dir,
            Path::new(r"C:\Program Files\Riekko\tun2socks.exe"),
            Some((&req, 9797)),
        );
        std::fs::write(dir.join("up.ps1"), up).unwrap();
        std::fs::write(dir.join("watchdog.ps1"), watchdog).unwrap();
        std::fs::write(dir.join("teardown.ps1"), teardown).unwrap();
    }
}
