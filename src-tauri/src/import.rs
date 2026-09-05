use crate::state::{Profile, UNGROUPED_ID};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use percent_encoding::percent_decode_str;
use url::Url;

/// Parses a single share link (`vless://...`, `hysteria2://...` / `hy2://...`)
/// into a profile. Freshly imported single keys land in `Ungrouped`; the
/// caller reassigns `group_id` afterwards for subscription imports.
pub fn parse_uri(raw: &str, id_seed: u64) -> Result<Profile, String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|_| "Не удалось разобрать ссылку".to_string())?;

    match url.scheme() {
        "vless" => parse_vless(&url, trimmed, id_seed),
        "hysteria2" | "hy2" => parse_hysteria2(&url, trimmed, id_seed),
        other => Err(format!("Протокол \"{other}\" пока не поддерживается")),
    }
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn host_port(url: &Url) -> Result<(String, u16), String> {
    let host = url
        .host_str()
        .ok_or_else(|| "В ссылке не указан хост".to_string())?;
    let port = url
        .port()
        .ok_or_else(|| "В ссылке не указан порт".to_string())?;
    Ok((host.to_string(), port))
}

fn remark(url: &Url, fallback: String) -> String {
    match url.fragment() {
        Some(f) if !f.is_empty() => decode(f),
        _ => fallback,
    }
}

fn transport_of(url: &Url, default: &str) -> String {
    url.query_pairs()
        .find(|(key, _)| key == "type")
        .map(|(_, value)| value.to_uppercase())
        .unwrap_or_else(|| default.to_string())
}

fn parse_vless(url: &Url, raw: &str, id_seed: u64) -> Result<Profile, String> {
    if url.username().is_empty() {
        return Err("В ссылке VLESS отсутствует UUID".to_string());
    }
    let (host, port) = host_port(url)?;
    let endpoint = format!("{host}:{port}");
    let name = remark(url, format!("VLESS {host}"));
    let transport = transport_of(url, "TCP");

    Ok(Profile {
        id: format!("profile-{id_seed}"),
        name,
        endpoint,
        transport,
        protocol: "VLESS".into(),
        group_id: UNGROUPED_ID.into(),
        uri: Some(raw.to_string()),
    })
}

fn parse_hysteria2(url: &Url, raw: &str, id_seed: u64) -> Result<Profile, String> {
    if url.username().is_empty() && url.password().is_none() {
        return Err("В ссылке Hysteria2 отсутствует пароль".to_string());
    }
    let (host, port) = host_port(url)?;
    let endpoint = format!("{host}:{port}");
    let name = remark(url, format!("Hysteria2 {host}"));

    Ok(Profile {
        id: format!("profile-{id_seed}"),
        name,
        endpoint,
        transport: "UDP".into(),
        protocol: "HYSTERIA2".into(),
        group_id: UNGROUPED_ID.into(),
        uri: Some(raw.to_string()),
    })
}

fn is_supported_scheme(line: &str) -> bool {
    line.starts_with("vless://") || line.starts_with("hysteria2://") || line.starts_with("hy2://")
}

/// Subscriptions are usually either a plain list of links (one per line) or
/// the whole body base64-encoded down to a single blob. Try the plain form
/// first since it's cheap to detect, fall back to base64.
fn lines_from_body(body: &str) -> Vec<String> {
    let plain: Vec<String> = body
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if plain.iter().any(|l| is_supported_scheme(l)) {
        return plain;
    }

    let cleaned: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(bytes) = STANDARD.decode(&cleaned) {
        if let Ok(decoded) = String::from_utf8(bytes) {
            return decoded
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
        }
    }

    plain
}

/// Parses every recognizable key out of a subscription body. Lines that
/// don't parse (comments, unsupported protocols) are silently skipped —
/// the caller reports how many profiles actually landed.
pub fn parse_subscription_body(body: &str, id_seed_start: u64) -> Vec<Profile> {
    lines_from_body(body)
        .into_iter()
        .enumerate()
        .filter_map(|(i, line)| parse_uri(&line, id_seed_start.wrapping_add(i as u64)).ok())
        .collect()
}

/// Derives a group name from a subscription URL's last path segment, minus
/// its extension — `.../my-servers.txt` becomes `my-servers`.
pub fn group_name_from_url(url: &Url) -> String {
    let last_segment = url
        .path_segments()
        .and_then(|segments| segments.filter(|s| !s.is_empty()).last())
        .map(decode);

    match last_segment {
        Some(raw_name) => match raw_name.rsplit_once('.') {
            Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
            _ => raw_name,
        },
        None => url.host_str().unwrap_or("Subscription").to_string(),
    }
}

/// Full connection parameters for a VLESS outbound, matching the fields the
/// Xray-core config format understands (https://xtls.github.io/config/outbounds/vless.html).
pub struct VlessParams {
    pub host: String,
    pub port: u16,
    pub uuid: String,
    pub flow: Option<String>,
    /// Transport: tcp/ws/grpc/...
    pub network: String,
    /// none/tls/reality
    pub security: String,
    pub sni: Option<String>,
    pub fingerprint: Option<String>,
    pub allow_insecure: bool,
    pub path: Option<String>,
    pub host_header: Option<String>,
    pub service_name: Option<String>,
    pub public_key: Option<String>,
    pub short_id: Option<String>,
}

/// Full connection parameters for a Hysteria2 client, matching the official
/// client config (https://v2.hysteria.network/docs/getting-started/Client/).
pub struct Hysteria2Params {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub sni: Option<String>,
    pub insecure: bool,
    pub obfs_password: Option<String>,
}

pub enum ConnectParams {
    Vless(VlessParams),
    Hysteria2(Hysteria2Params),
}

/// Re-parses a stored share link into everything a real client core needs to
/// actually dial the server, as opposed to `parse_uri`'s lightweight profile
/// used just for the list UI.
pub fn parse_connect_params(raw: &str) -> Result<ConnectParams, String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|_| "Не удалось разобрать ссылку".to_string())?;

    match url.scheme() {
        "vless" => Ok(ConnectParams::Vless(parse_vless_full(&url)?)),
        "hysteria2" | "hy2" => Ok(ConnectParams::Hysteria2(parse_hysteria2_full(&url)?)),
        other => Err(format!("Протокол \"{other}\" пока не поддерживается")),
    }
}

fn query_get(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}

fn parse_vless_full(url: &Url) -> Result<VlessParams, String> {
    if url.username().is_empty() {
        return Err("В ссылке VLESS отсутствует UUID".to_string());
    }
    let (host, port) = host_port(url)?;
    let network = query_get(url, "type").unwrap_or_else(|| "tcp".to_string());
    let security = query_get(url, "security").unwrap_or_else(|| "none".to_string());
    let allow_insecure = matches!(
        query_get(url, "allowInsecure").as_deref(),
        Some("1") | Some("true")
    );

    Ok(VlessParams {
        host,
        port,
        uuid: decode(url.username()),
        flow: query_get(url, "flow"),
        network,
        security,
        sni: query_get(url, "sni"),
        fingerprint: query_get(url, "fp"),
        allow_insecure,
        path: query_get(url, "path"),
        host_header: query_get(url, "host"),
        service_name: query_get(url, "serviceName"),
        public_key: query_get(url, "pbk"),
        short_id: query_get(url, "sid"),
    })
}

fn parse_hysteria2_full(url: &Url) -> Result<Hysteria2Params, String> {
    let password = if !url.username().is_empty() {
        decode(url.username())
    } else if let Some(pw) = url.password() {
        decode(pw)
    } else {
        return Err("В ссылке Hysteria2 отсутствует пароль".to_string());
    };
    let (host, port) = host_port(url)?;
    let insecure = matches!(query_get(url, "insecure").as_deref(), Some("1") | Some("true"));

    Ok(Hysteria2Params {
        host,
        port,
        password,
        sni: query_get(url, "sni"),
        insecure,
        obfs_password: query_get(url, "obfs-password"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vless_link() {
        let profile = parse_uri(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@nl.example.net:443?type=ws&security=tls#My%20Server",
            1,
        )
        .unwrap();
        assert_eq!(profile.protocol, "VLESS");
        assert_eq!(profile.name, "My Server");
        assert_eq!(profile.endpoint, "nl.example.net:443");
        assert_eq!(profile.transport, "WS");
        assert_eq!(profile.group_id, UNGROUPED_ID);
    }

    #[test]
    fn parses_hysteria2_link_with_hy2_scheme() {
        let profile = parse_uri("hy2://s3cr3t@de.example.net:8443?insecure=1#Berlin", 2).unwrap();
        assert_eq!(profile.protocol, "HYSTERIA2");
        assert_eq!(profile.name, "Berlin");
        assert_eq!(profile.endpoint, "de.example.net:8443");
        assert_eq!(profile.transport, "UDP");
    }

    #[test]
    fn falls_back_to_host_when_no_remark() {
        let profile = parse_uri("vless://uuid@1.2.3.4:443", 3).unwrap();
        assert_eq!(profile.name, "VLESS 1.2.3.4");
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert!(parse_uri("ss://aes-256-gcm@1.2.3.4:8080#x", 4).is_err());
    }

    #[test]
    fn rejects_vless_without_uuid() {
        assert!(parse_uri("vless://@nl.example.net:443", 5).is_err());
    }

    #[test]
    fn rejects_missing_port() {
        assert!(parse_uri("vless://uuid@nl.example.net", 6).is_err());
    }

    #[test]
    fn group_name_strips_extension() {
        let url = Url::parse("https://example.com/subs/my-servers.txt").unwrap();
        assert_eq!(group_name_from_url(&url), "my-servers");
    }

    #[test]
    fn group_name_handles_no_extension() {
        let url = Url::parse("https://example.com/subs/plain").unwrap();
        assert_eq!(group_name_from_url(&url), "plain");
    }

    #[test]
    fn group_name_falls_back_to_host() {
        let url = Url::parse("https://example.com/").unwrap();
        assert_eq!(group_name_from_url(&url), "example.com");
    }

    #[test]
    fn parses_plain_subscription_body() {
        let body = "vless://uuid@a.example.net:443#A\nhy2://pw@b.example.net:8443#B\n# comment\nnot-a-link";
        let profiles = parse_subscription_body(body, 100);
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn parses_base64_subscription_body() {
        let raw = "vless://uuid@a.example.net:443#A\nhy2://pw@b.example.net:8443#B";
        let encoded = STANDARD.encode(raw);
        let profiles = parse_subscription_body(&encoded, 200);
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn parses_full_vless_reality_params() {
        let params = match parse_connect_params(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@nl.example.net:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=example.com&fp=chrome&pbk=abc123&sid=de&type=tcp#Reality",
        )
        .unwrap()
        {
            ConnectParams::Vless(p) => p,
            _ => panic!("expected vless"),
        };
        assert_eq!(params.host, "nl.example.net");
        assert_eq!(params.port, 443);
        assert_eq!(params.uuid, "b831381d-6324-4d53-ad4f-8cda48b30811");
        assert_eq!(params.flow.as_deref(), Some("xtls-rprx-vision"));
        assert_eq!(params.security, "reality");
        assert_eq!(params.sni.as_deref(), Some("example.com"));
        assert_eq!(params.public_key.as_deref(), Some("abc123"));
        assert_eq!(params.short_id.as_deref(), Some("de"));
    }

    #[test]
    fn parses_full_vless_ws_tls_params() {
        let params = match parse_connect_params(
            "vless://uuid@nl.example.net:443?type=ws&security=tls&path=%2Fws&host=cdn.example.com#WS",
        )
        .unwrap()
        {
            ConnectParams::Vless(p) => p,
            _ => panic!("expected vless"),
        };
        assert_eq!(params.network, "ws");
        assert_eq!(params.security, "tls");
        assert_eq!(params.path.as_deref(), Some("/ws"));
        assert_eq!(params.host_header.as_deref(), Some("cdn.example.com"));
    }

    #[test]
    fn parses_full_hysteria2_params() {
        let params = match parse_connect_params(
            "hysteria2://s3cr3t@de.example.net:8443?insecure=1&sni=example.com&obfs=salamander&obfs-password=hunter2#Berlin",
        )
        .unwrap()
        {
            ConnectParams::Hysteria2(p) => p,
            _ => panic!("expected hysteria2"),
        };
        assert_eq!(params.host, "de.example.net");
        assert_eq!(params.port, 8443);
        assert_eq!(params.password, "s3cr3t");
        assert!(params.insecure);
        assert_eq!(params.sni.as_deref(), Some("example.com"));
        assert_eq!(params.obfs_password.as_deref(), Some("hunter2"));
    }
}
