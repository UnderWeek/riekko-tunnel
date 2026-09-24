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

/// Per-session values baked into the privileged scripts.
struct Session<'a> {
    req: &'a TunRequest<'a>,
    rest_port: u16,
    /// Bearer token for tun2socks's REST API. It runs as root/admin and
    /// would otherwise let any local process (or a web page probing
    /// localhost ports) list and kill every connection.
    rest_token: &'a str,
}

/// A running tunnel. Dropping it tears the tunnel down.
pub struct TunHandle {
    work_dir: PathBuf,
    /// Where the privileged side keeps its state (status, pids).
    run_dir: PathBuf,
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
            Err(_) => platform::elevated_teardown(&self.run_dir, &self.work_dir),
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
        // The watchdog only ever *reads* the stop file (privileged code
        // doesn't delete files in a user-writable folder); it acknowledges
        // by writing its final status word once the teardown is done.
        let deadline = Instant::now() + STOP_ACK_TIMEOUT;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            let word = read_status(&self.status_file).0;
            if matches!(word.as_deref(), Some("stopped" | "dead")) || !self.stop_file.exists() {
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
    rest_token: String,
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
            if let Ok(response) = client.get(&url).bearer_auth(&rest_token).send().await {
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
    let rest_token = sys::random_token();
    let tun2socks = platform::tun2socks_path(req.work_dir)?;

    let session = Session {
        req: &req,
        rest_port,
        rest_token: &rest_token,
    };
    let run_dir = platform::up(&session, &tun2socks, &stop_file)?;

    let up_bytes = Arc::new(AtomicU64::new(0));
    let down_bytes = Arc::new(AtomicU64::new(0));
    let watcher_stop = Arc::new(AtomicBool::new(false));
    spawn_traffic_watcher(
        rest_port,
        rest_token.clone(),
        up_bytes.clone(),
        down_bytes.clone(),
        watcher_stop.clone(),
    );

    Ok(TunHandle {
        work_dir: req.work_dir.to_path_buf(),
        status_file: run_dir.join("status"),
        run_dir,
        stop_file,
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
REST_TOKEN=@@REST_TOKEN@@
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
rm -f "$RUN/watchdog.pid"
device_free || fail "The TUN device $DEV is held by another process"
if [ -n "$SERVER_IP" ]; then printf '%s\n' "$SERVER_IP" > "$RUN/server_ip"; fi
$DETACH "$T2S" --device "$DEV" --proxy "socks5://127.0.0.1:$SOCKS_PORT" --restapi "$REST_TOKEN@127.0.0.1:$REST_PORT" --loglevel warning </dev/null >"$RUN/tun2socks.log" 2>&1 &
echo $! > "$RUN/tun2socks.pid"
t2s_log() { tail -c 400 "$RUN/tun2socks.log" 2>/dev/null; }
device_up || fail "TUN device did not come up: $(t2s_log)"
routes_up || fail "Could not install routes"
routes_ok || fail "Routes did not take effect"
is_t2s "$(cat "$RUN/tun2socks.pid")" || fail "tun2socks exited: $(t2s_log)"
cat > "$RUN/watchdog.sh" <<'RIEKKO_WATCHDOG_EOF'
@@WATCHDOG@@
RIEKKO_WATCHDOG_EOF
set_status ok
# No nohup: a non-interactive shell sends no SIGHUP to background jobs (and
# the watchdog ignores it anyway), while macOS's nohup can refuse to run
# outside a console session.
$DETACH /bin/bash "$RUN/watchdog.sh" </dev/null >/dev/null 2>&1 &
echo $! > "$RUN/watchdog.pid"
sleep 0.3
alive "$(cat "$RUN/watchdog.pid")" || fail "The watchdog did not start"
exit 0
"#;

#[cfg(unix)]
const UNIX_WATCHDOG: &str = r#"
trap '' HUP
echo $$ > "$RUN/watchdog.pid"
finish() { teardown; set_status "$1"; rm -f "$RUN/watchdog.pid"; exit 0; }
while :; do
  # Only tested, never deleted: root doesn't remove files in a folder the
  # user controls. "stopped" in the status file is the acknowledgement.
  if [ -e "$STOP" ]; then finish stopped; fi
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
rm -f "$RUN/watchdog.pid"
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
        session: Option<&Session>,
    ) -> Self {
        let (server_ip, socks_port, rest_port, rest_token, core_pid) = match session {
            Some(s) => (
                s.req.server_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                s.req.socks_port,
                s.rest_port,
                s.rest_token.to_string(),
                s.req.core_pid,
            ),
            None => (String::new(), 0, 0, String::new(), 0),
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
            ("REST_TOKEN", sys::sh_quote(&rest_token)),
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
  # Gateway *and* interface: undocking can move the default route to
  # another interface that happens to use the same gateway address.
  printf '%s %s\n' "$GW" "$IF" > "$RUN/server_gw"
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
stale_t2s() {
  # tun2socks processes (by executable, not by matching our own script's
  # command line) still holding our device, e.g. from an older build.
  ps -axo pid=,comm= 2>/dev/null | awk '$2 ~ /tun2socks$/ {print $1}' | while read -r p; do
    if cmd_of "$p" | grep -q -- "--device $DEV"; then echo "$p"; fi
  done
}
device_free() {
  ifconfig "$DEV" >/dev/null 2>&1 || return 0
  for p in $(stale_t2s); do kill "$p" 2>/dev/null; done
  i=0
  while ifconfig "$DEV" >/dev/null 2>&1; do
    i=$((i+1)); [ "$i" -gt 30 ] && return 1
    sleep 0.1
  done
}
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
    NOW="$(default_gw) $(default_if)"
    PINNED=$(cat "$RUN/server_gw" 2>/dev/null || true)
    if [ "$SIF" = "$DEV" ] || { [ "$NOW" != " " ] && [ "$NOW" != "$PINNED" ]; }; then add_server_route; fi
  fi
  routes_ok || add_nets
}
"#;

    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        sidecar_path("tun2socks")
    }

    fn scripts(stop_file: &Path, tun2socks: &Path, session: Option<&Session>) -> UnixScripts {
        UnixScripts::new(
            FUNCTIONS, RUN_DIR, stop_file, tun2socks, DEVICE, "", session,
        )
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

    pub fn up(session: &Session, tun2socks: &Path, stop_file: &Path) -> Result<PathBuf, String> {
        run_elevated(&scripts(stop_file, tun2socks, Some(session)).up())?;
        Ok(PathBuf::from(RUN_DIR))
    }

    pub fn elevated_teardown(_run_dir: &Path, work_dir: &Path) -> Result<(), String> {
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
        let session = Session {
            req: &req,
            rest_port: 9797,
            rest_token: "0123abcd",
        };
        let s = scripts(
            Path::new("/tmp/riekko test's dir/stop"),
            Path::new("/Applications/Riekko Tunnel.app/Contents/MacOS/tun2socks"),
            Some(&session),
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
device_free() { device_down; }
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

    fn scripts(stop_file: &Path, tun2socks: &Path, session: Option<&Session>) -> UnixScripts {
        // setsid: a Ctrl-C in the terminal the app was started from must
        // not reach root's tun2socks/watchdog and strand the routes.
        UnixScripts::new(
            FUNCTIONS, RUN_DIR, stop_file, tun2socks, DEVICE, "setsid", session,
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

    pub fn up(session: &Session, tun2socks: &Path, stop_file: &Path) -> Result<PathBuf, String> {
        run_elevated(&scripts(stop_file, tun2socks, Some(session)).up())?;
        Ok(PathBuf::from(RUN_DIR))
    }

    pub fn elevated_teardown(_run_dir: &Path, work_dir: &Path) -> Result<(), String> {
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
        let session = Session {
            req: &req,
            rest_port: 9797,
            rest_token: "0123abcd",
        };
        let s = scripts(
            Path::new("/tmp/riekko test's dir/stop"),
            Path::new("/opt/Riekko Tunnel/tun2socks"),
            Some(&session),
        );
        (s.up(), s.teardown())
    }
}

// ---------------------------------------------------------------------------
// Windows: PowerShell scripts run elevated via UAC.
// ---------------------------------------------------------------------------

/// Shared by the "up" script, the watchdog and the fallback teardown.
#[cfg(any(windows, test))]
const WIN_PRELUDE: &str = r#"$Run = @@RUN@@
$Stop = @@STOP@@
$T2S = @@T2S@@
$Dev = 'RiekkoTun'
$TunIp = '@@TUN_IP@@'
$ServerIp = @@SERVER_IP@@
$Socks = @@SOCKS_PORT@@
$Rest = @@REST_PORT@@
$RestToken = @@REST_TOKEN@@
$AppPid = @@APP_PID@@
$AppName = @@APP_NAME@@
$CorePid = @@CORE_PID@@

function Set-Status([string]$s) {
  try {
    $tmp = Join-Path $Run 'status.tmp'
    [IO.File]::WriteAllText($tmp, $s)
    Move-Item -LiteralPath $tmp -Destination (Join-Path $Run 'status') -Force
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
}

function Read-First([string]$dir, [string]$name) {
  $path = Join-Path $dir $name
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
  Get-Content -LiteralPath $path -ErrorAction SilentlyContinue | Select-Object -First 1
}

function Stop-Tunnel([string]$dir) {
  $ErrorActionPreference = 'SilentlyContinue'
  $t = Read-First $dir 'tun2socks.pid'
  if ($t) { Get-Process -Id ([int]$t) | Where-Object { $_.ProcessName -like 'tun2socks*' } | Stop-Process -Force }
  $old = Read-First $dir 'server_ip'
  if ($old) { Remove-NetRoute -DestinationPrefix "$old/32" -PolicyStore ActiveStore -Confirm:$false }
  foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1')) {
    netsh interface ipv4 delete route $prefix $Dev store=active | Out-Null
  }
  Remove-Item -LiteralPath (Join-Path $dir 'tun2socks.pid'), (Join-Path $dir 'server_ip') -Force
}

function Stop-Watchdog([string]$dir) {
  $ErrorActionPreference = 'SilentlyContinue'
  $w = Read-First $dir 'watchdog.pid'
  if ($w -and [int]$w -ne $PID) {
    Get-Process -Id ([int]$w) | Where-Object { $_.ProcessName -eq 'powershell' } | Stop-Process -Force
  }
}
"#;

#[cfg(any(windows, test))]
const WIN_UP: &str = r#"
$ErrorActionPreference = 'Stop'
$WatchdogCommand = '@@WATCHDOG@@'
function Fail([string]$msg) {
  try { [IO.File]::WriteAllText((Join-Path $Run 'error.txt'), $msg) } catch {}
  Stop-Tunnel $Run
  Set-Status 'dead'
  exit 1
}

# Everything the elevated side writes lives in a fresh directory with an
# unpredictable name that only Administrators and SYSTEM can modify, so a
# user-level process can't swap a file (or plant a junction) under it.
try { New-Item -ItemType Directory -Path $Run -ErrorAction Stop | Out-Null } catch { exit 4 }
icacls $Run /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
if ($LASTEXITCODE -ne 0) { exit 4 }

try {
  # Earlier sessions (normally long gone): stop whatever they left running,
  # then delete their plain files only — never recursing, never following
  # a reparse point.
  $parent = Split-Path -Parent $Run
  Get-ChildItem -LiteralPath $parent -Directory -Filter 'RiekkoTunnel-*' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -ne $Run -and -not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) } |
    ForEach-Object {
      Stop-Watchdog $_.FullName
      Stop-Tunnel $_.FullName
      Get-ChildItem -LiteralPath $_.FullName -File -Force -ErrorAction SilentlyContinue |
        Where-Object { -not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) } |
        Remove-Item -Force -ErrorAction SilentlyContinue
      Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
    }

  $main = $null
  if ($ServerIp) {
    $main = Get-MainRoute
    if (-not $main) { Fail 'No active default route to reach the server through' }
    [IO.File]::WriteAllText((Join-Path $Run 'server_ip'), $ServerIp)
  }

  $p = Start-Process -FilePath $T2S -WindowStyle Hidden -PassThru -ArgumentList @(
    '--device', $Dev, '--proxy', "socks5://127.0.0.1:$Socks",
    '--restapi', "$RestToken@127.0.0.1:$Rest", '--loglevel', 'warning')
  [IO.File]::WriteAllText((Join-Path $Run 'tun2socks.pid'), [string]$p.Id)

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
  # Started from memory (-EncodedCommand), not from a script file the user
  # could replace before the elevated process reads it.
  $w = Start-Process -FilePath 'powershell.exe' -WindowStyle Hidden -PassThru -ArgumentList @(
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden',
    '-EncodedCommand', $WatchdogCommand)
  [IO.File]::WriteAllText((Join-Path $Run 'watchdog.pid'), [string]$w.Id)
  exit 0
} catch {
  Fail $_.Exception.Message
}
"#;

#[cfg(any(windows, test))]
const WIN_WATCHDOG: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'

function Test-AppAlive {
  # By name too: Windows reuses PIDs, and a recycled one must not keep a
  # dead app's tunnel up.
  $p = Get-Process -Id $AppPid
  return [bool]($p -and (-not $AppName -or $p.ProcessName -eq $AppName))
}

function Test-Tun2socksAlive {
  $t = Read-First $Run 'tun2socks.pid'
  if (-not $t) { return $false }
  return [bool](Get-Process -Id ([int]$t) | Where-Object { $_.ProcessName -like 'tun2socks*' })
}

# Windows drops active-store routes of an interface that disconnects (sleep,
# a Wi-Fi reconnect), even when it comes back with the same gateway. Without
# the server's own /32 the core's traffic loops into the tunnel. Returns
# whether everything was already in place.
function Repair-Routes {
  $ok = $true
  if ($ServerIp) {
    $main = Get-MainRoute
    $pinned = @(Get-NetRoute -DestinationPrefix "$ServerIp/32" -PolicyStore ActiveStore)
    $have = if ($pinned.Count) { "$($pinned[0].ifIndex)|$($pinned[0].NextHop)" } else { '' }
    if ($main -and $have -ne "$($main.ifIndex)|$($main.NextHop)") {
      try { Add-ServerRoute $main } catch {}
      $ok = $false
    }
    if (-not @(Get-NetRoute -DestinationPrefix "$ServerIp/32" -PolicyStore ActiveStore).Count) { $ok = $false }
  }
  foreach ($prefix in @('0.0.0.0/1', '128.0.0.0/1')) {
    $routes = @(Get-NetRoute -DestinationPrefix $prefix -PolicyStore ActiveStore | Where-Object { $_.InterfaceAlias -eq $Dev })
    if (-not $routes.Count) {
      netsh interface ipv4 add route $prefix $Dev $TunIp metric=1 store=active | Out-Null
      $ok = $false
    }
  }
  return $ok
}

while ($true) {
  # The stop file is only tested, never touched: "stopped" in the status
  # file is the acknowledgement.
  if (Test-Path -LiteralPath $Stop) { Stop-Tunnel $Run; Set-Status 'stopped'; exit 0 }
  if (-not (Test-AppAlive)) {
    if ($CorePid) {
      Get-Process -Id $CorePid | Where-Object { $_.ProcessName -match '^(xray|hysteria)' } | Stop-Process -Force
    }
    Stop-Tunnel $Run; Set-Status 'stopped'; exit 0
  }
  if (-not (Test-Tun2socksAlive)) { Stop-Tunnel $Run; Set-Status 'dead'; exit 0 }
  if (Repair-Routes) { Set-Status 'ok' } else { Set-Status 'degraded' }
  Start-Sleep -Seconds 1
}
"#;

#[cfg(any(windows, test))]
const WIN_TEARDOWN: &str = r#"
Stop-Watchdog $Run
Stop-Tunnel $Run
Set-Status 'stopped'
exit 0
"#;

/// Renders the Windows scripts: `(up, teardown)`. The watchdog is embedded
/// in `up` as an `-EncodedCommand` payload.
#[cfg(any(windows, test))]
fn windows_scripts(
    run_dir: &Path,
    stop_file: &Path,
    tun2socks: &Path,
    session: Option<&Session>,
) -> (String, String) {
    let (server_ip, socks_port, rest_port, rest_token, core_pid) = match session {
        Some(s) => (
            s.req.server_ip.map(|ip| ip.to_string()).unwrap_or_default(),
            s.req.socks_port,
            s.rest_port,
            s.rest_token.to_string(),
            s.req.core_pid,
        ),
        None => (String::new(), 0, 0, String::new(), 0),
    };
    let app_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let vars = [
        ("RUN", sys::ps_quote(&run_dir.to_string_lossy())),
        ("STOP", sys::ps_quote(&stop_file.to_string_lossy())),
        ("T2S", sys::ps_quote(&tun2socks.to_string_lossy())),
        ("TUN_IP", TUN_IP.to_string()),
        ("SERVER_IP", sys::ps_quote(&server_ip)),
        ("SOCKS_PORT", socks_port.to_string()),
        ("REST_PORT", rest_port.to_string()),
        ("REST_TOKEN", sys::ps_quote(&rest_token)),
        ("APP_PID", std::process::id().to_string()),
        ("APP_NAME", sys::ps_quote(&app_name)),
        ("CORE_PID", core_pid.to_string()),
    ];
    let prelude = sys::render(WIN_PRELUDE, &vars);
    let watchdog = sys::ps_encode(&format!("{prelude}{WIN_WATCHDOG}"));
    let up = sys::render(WIN_UP, &[("WATCHDOG", watchdog)]);
    (format!("{prelude}{up}"), format!("{prelude}{WIN_TEARDOWN}"))
}

/// The text a script file will hold (CRLF line endings, as Windows
/// PowerShell expects) and its SHA-256 as uppercase hex — what the elevated
/// bootstrap checks before running it.
#[cfg(any(windows, test))]
fn windows_script_text(script: &str) -> (String, String) {
    use sha2::{Digest, Sha256};
    let text = script.replace("\r\n", "\n").replace('\n', "\r\n");
    let hash = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    (text, hash)
}

/// The elevated entry point, passed as `-EncodedCommand`. It reads the
/// script file once, refuses to run it unless it hashes to what the app
/// wrote, and then runs exactly the text it checked — a same-user process
/// swapping the file between write and elevation gets nowhere.
#[cfg(any(windows, test))]
fn windows_bootstrap(script_path: &Path, hash: &str) -> String {
    format!(
        "$c = [IO.File]::ReadAllText({path})\n\
         $h = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes($c))).Replace('-', '')\n\
         if ($h -ne '{hash}') {{ exit 3 }}\n\
         & ([scriptblock]::Create($c))\n\
         exit $LASTEXITCODE\n",
        path = sys::ps_quote(&script_path.to_string_lossy()),
    )
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    const ADAPTER: &str = "RiekkoTun";
    /// ERROR_CANCELLED — the runner's exit code when UAC is declined.
    const UAC_CANCELLED: i32 = 1223;
    const HASH_MISMATCH: i32 = 3;
    const NO_RUN_DIR: i32 = 4;

    fn program_data() -> PathBuf {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
    }

    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        sidecar_path("tun2socks")
    }

    /// Writes the script with a UTF-8 BOM — Windows PowerShell 5.1 reads
    /// BOM-less files in the ANSI code page, mangling non-ASCII paths (a
    /// Cyrillic user name, say) — and returns its hash.
    fn write_script(path: &Path, script: &str) -> Result<String, String> {
        let (text, hash) = windows_script_text(script);
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(text.as_bytes());
        let _ = std::fs::remove_file(path);
        crate::engine::write_private(path, &bytes)
            .map_err(|e| format!("Не удалось записать скрипт: {e}"))?;
        Ok(hash)
    }

    /// Runs a script file elevated via UAC (through the hash-checking
    /// bootstrap) and waits for it.
    fn run_elevated(script_path: &Path, hash: &str) -> Result<(), String> {
        let bootstrap = sys::ps_encode(&windows_bootstrap(script_path, hash));
        let runner = format!(
            "try {{\n\
               $p = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WindowStyle Hidden -PassThru -ErrorAction Stop \
                 -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-EncodedCommand','{bootstrap}')\n\
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
            Some(HASH_MISMATCH) => Err("скрипт TUN был изменён перед запуском".to_string()),
            Some(NO_RUN_DIR) => Err("не удалось создать служебную папку в ProgramData".to_string()),
            _ => Err("сценарий завершился с ошибкой".to_string()),
        }
    }

    pub fn up(session: &Session, tun2socks: &Path, stop_file: &Path) -> Result<PathBuf, String> {
        let run_dir = program_data().join(format!("RiekkoTunnel-{}", sys::random_token()));
        let (up, _) = windows_scripts(&run_dir, stop_file, tun2socks, Some(session));
        let up_path = session.req.work_dir.join("up.ps1");
        let hash = write_script(&up_path, &up)?;

        let result = run_elevated(&up_path, &hash);
        let _ = std::fs::remove_file(&up_path);
        // The status file is the source of truth, not the exit code (which
        // `Start-Process -Verb RunAs` doesn't always relay): the elevated
        // script only writes "ok" once the adapter and routes are in place,
        // and from then on the watchdog owns the tunnel.
        let status = read_status(&run_dir.join("status"));
        if matches!(status, (Some(ref w), true) if w == "ok" || w == "degraded") {
            return Ok(run_dir);
        }
        let detail = std::fs::read_to_string(run_dir.join("error.txt"))
            .map(|s| s.trim_start_matches('\u{feff}').trim().to_string())
            .unwrap_or_default();
        match result {
            Err(e) if e.contains("UAC") => Err(e),
            Err(e) if detail.is_empty() => Err(format!("Настройка TUN не удалась: {e}")),
            _ => Err(format!(
                "Настройка TUN не удалась: {}",
                if detail.is_empty() {
                    "неизвестная ошибка".to_string()
                } else {
                    detail
                }
            )),
        }
    }

    pub fn elevated_teardown(run_dir: &Path, work_dir: &Path) -> Result<(), String> {
        let (_, teardown) = windows_scripts(
            run_dir,
            &work_dir.join("stop"),
            Path::new("tun2socks.exe"),
            None,
        );
        let path = work_dir.join("down.ps1");
        let hash = write_script(&path, &teardown)?;
        let result = run_elevated(&path, &hash);
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

    pub fn tun2socks_path(_work_dir: &Path) -> Result<PathBuf, String> {
        Err("TUN-режим не поддерживается на этой платформе".to_string())
    }
    pub fn up(_session: &Session, _t2s: &Path, _stop: &Path) -> Result<PathBuf, String> {
        Err("TUN-режим не поддерживается на этой платформе".to_string())
    }
    pub fn elevated_teardown(_run_dir: &Path, _work_dir: &Path) -> Result<(), String> {
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
            run_dir: std::env::temp_dir(),
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

    fn windows_test_scripts() -> (String, String, String) {
        let req = TunRequest {
            socks_port: 10808,
            server_ip: Some(Ipv4Addr::new(203, 0, 113, 7)),
            core_pid: 4242,
            work_dir: Path::new(r"C:\Users\O'Brien\AppData\Local\com.riekko.tunnel\tun"),
        };
        let session = Session {
            req: &req,
            rest_port: 9797,
            rest_token: "0123abcd",
        };
        let (up, teardown) = windows_scripts(
            Path::new(r"C:\ProgramData\RiekkoTunnel-00ff"),
            Path::new(r"C:\Users\O'Brien\AppData\Local\com.riekko.tunnel\tun\stop"),
            Path::new(r"C:\Program Files\Riekko Tunnel\tun2socks.exe"),
            Some(&session),
        );
        // Recover the watchdog from its -EncodedCommand payload.
        let start = up.find("$WatchdogCommand = '").unwrap() + "$WatchdogCommand = '".len();
        let end = up[start..].find('\'').unwrap() + start;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&up[start..end])
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        (up, String::from_utf16(&units).unwrap(), teardown)
    }

    #[test]
    fn windows_scripts_have_no_unfilled_placeholders() {
        let (up, watchdog, teardown) = windows_test_scripts();
        for script in [&up, &watchdog, &teardown] {
            assert!(!script.contains("@@"));
            assert!(script.contains(r"$Run = 'C:\ProgramData\RiekkoTunnel-00ff'"));
            assert!(script
                .contains(r"$Stop = 'C:\Users\O''Brien\AppData\Local\com.riekko.tunnel\tun\stop'"));
        }
        assert!(up.contains("$ServerIp = '203.0.113.7'"));
        assert!(up.contains(r#""$RestToken@127.0.0.1:$Rest""#));
        assert!(watchdog.contains("$CorePid = 4242"));
        assert!(watchdog.contains("Repair-Routes"));
    }

    #[test]
    fn windows_bootstrap_hash_matches_the_script_text() {
        let (text, hash) = windows_script_text("Write-Host 'hi'\nexit 0\n");
        assert_eq!(text, "Write-Host 'hi'\r\nexit 0\r\n");
        assert_eq!(hash.len(), 64);
        let bootstrap = windows_bootstrap(Path::new(r"C:\Users\O'Brien\up.ps1"), &hash);
        assert!(bootstrap.contains(&format!("if ($h -ne '{hash}') {{ exit 3 }}")));
        assert!(bootstrap.contains(r"ReadAllText('C:\Users\O''Brien\up.ps1')"));
        // Idempotent on text that already has CRLF.
        assert_eq!(windows_script_text(&text).1, hash);
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
        let (up, watchdog, teardown) = windows_test_scripts();
        std::fs::write(dir.join("up.ps1"), up).unwrap();
        std::fs::write(dir.join("watchdog.ps1"), watchdog).unwrap();
        std::fs::write(dir.join("teardown.ps1"), teardown).unwrap();
        let (text, hash) = windows_script_text("Write-Host 'bootstrapped'\nexit 5\n");
        let script = dir.join("boot-target.ps1");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(text.as_bytes());
        std::fs::write(&script, bytes).unwrap();
        std::fs::write(dir.join("bootstrap.ps1"), windows_bootstrap(&script, &hash)).unwrap();
    }
}
