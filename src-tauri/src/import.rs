use crate::state::{unique_id, Profile, UNGROUPED_ID};
use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use url::{Host, Url};

/// Parses a single share link (`vless://...`, `hysteria2://...` / `hy2://...`)
/// into a profile. Freshly imported single keys land in `Ungrouped`; the
/// caller reassigns `group_id` afterwards for subscription imports.
pub fn parse_uri(raw: &str) -> Result<Profile, String> {
    let trimmed = raw.trim();
    match parse_connect_params(trimmed)? {
        ConnectParams::Vless(p) => {
            let url = Url::parse(trimmed).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
            Ok(Profile {
                id: unique_id("profile"),
                name: remark(&url, format!("VLESS {}", p.host)),
                endpoint: join_host_port(&p.host, &p.port.to_string()),
                transport: p.network.to_uppercase(),
                protocol: "VLESS".into(),
                group_id: UNGROUPED_ID.into(),
                uri: Some(trimmed.to_string()),
            })
        }
        ConnectParams::Hysteria2(p) => {
            let (normalized, _) = split_port_spec(trimmed);
            let url =
                Url::parse(&normalized).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
            Ok(Profile {
                id: unique_id("profile"),
                name: remark(&url, format!("Hysteria2 {}", p.host)),
                endpoint: join_host_port(&p.host, &p.port_spec),
                transport: "UDP".into(),
                protocol: "HYSTERIA2".into(),
                group_id: UNGROUPED_ID.into(),
                uri: Some(trimmed.to_string()),
            })
        }
    }
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// The bare host, without the `[...]` that `Url::host_str` keeps around
/// IPv6 literals — Xray and the OS resolver both want the plain address.
fn host_of(url: &Url) -> Result<String, String> {
    match url.host() {
        Some(Host::Domain(d)) if !d.is_empty() => Ok(decode(d)),
        Some(Host::Ipv4(a)) => Ok(a.to_string()),
        Some(Host::Ipv6(a)) => Ok(a.to_string()),
        _ => Err("В ссылке не указан хост".to_string()),
    }
}

/// `host:port`, bracketing IPv6 literals so the result stays parseable.
pub fn join_host_port(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn remark(url: &Url, fallback: String) -> String {
    match url.fragment() {
        Some(f) if !f.trim().is_empty() => decode(f).trim().to_string(),
        _ => fallback,
    }
}

fn scheme_of(line: &str) -> Option<String> {
    line.split_once("://").map(|(s, _)| s.to_ascii_lowercase())
}

fn is_supported_scheme(line: &str) -> bool {
    matches!(
        scheme_of(line).as_deref(),
        Some("vless" | "hysteria2" | "hy2")
    )
}

/// Subscription providers disagree on base64 flavor: padded or not,
/// standard or URL-safe alphabet. Accept all of them.
fn decode_base64_text(text: &str) -> Option<String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    [alphabet::STANDARD, alphabet::URL_SAFE]
        .iter()
        .find_map(|a| GeneralPurpose::new(a, config).decode(&cleaned).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn non_empty_lines(text: &str) -> Vec<String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Subscriptions are usually either a plain list of links (one per line) or
/// the whole body base64-encoded down to a single blob. Try the plain form
/// first since it's cheap to detect, fall back to base64.
fn lines_from_body(body: &str) -> Vec<String> {
    let plain = non_empty_lines(body);
    if plain.iter().any(|l| is_supported_scheme(l)) {
        return plain;
    }
    match decode_base64_text(body.trim_start_matches('\u{feff}')) {
        Some(decoded) => non_empty_lines(&decoded),
        None => plain,
    }
}

/// Parses every recognizable key out of a subscription body. Lines that
/// don't parse (comments, unsupported protocols) are silently skipped —
/// the caller reports how many profiles actually landed.
pub fn parse_subscription_body(body: &str) -> Vec<Profile> {
    lines_from_body(body)
        .into_iter()
        .filter(|line| is_supported_scheme(line))
        .filter_map(|line| parse_uri(&line).ok())
        .collect()
}

/// Derives a group name from a subscription URL's last path segment, minus
/// its extension — `.../my-servers.txt` becomes `my-servers`. Segments that
/// every panel uses (`/api/v1/client/subscribe`, `/sub/<token>/v2ray`) or
/// that are just an access token say nothing, so those fall back to the
/// host name.
pub fn group_name_from_url(url: &Url) -> String {
    const GENERIC: [&str; 12] = [
        "sub",
        "subs",
        "subscribe",
        "subscription",
        "link",
        "links",
        "v2ray",
        "v2rayn",
        "clash",
        "singbox",
        "sing-box",
        "api",
    ];
    let host = || url.host_str().unwrap_or("Subscription").to_string();
    let Some(raw_name) = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .map(decode)
    else {
        return host();
    };
    let name = match raw_name.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
        _ => raw_name,
    };
    let looks_like_token = name.len() >= 16
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if looks_like_token || GENERIC.contains(&name.to_ascii_lowercase().as_str()) {
        host()
    } else {
        name
    }
}

/// Decodes a `profile-title` response header (Marzban, Remnawave, 3x-ui):
/// either plain text or `base64:<...>`.
pub fn decode_profile_title(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let title = match raw.strip_prefix("base64:") {
        Some(encoded) => decode_base64_text(encoded)?,
        None => raw.to_string(),
    };
    let title: String = title.trim().chars().take(64).collect();
    (!title.is_empty()).then_some(title)
}

/// Full connection parameters for a VLESS outbound, matching the fields the
/// Xray-core config format understands (https://xtls.github.io/config/outbounds/vless.html).
pub struct VlessParams {
    /// Server host as written in the link (domain or bare IP literal).
    pub host: String,
    pub port: u16,
    pub uuid: String,
    /// VLESS `encryption` — "none" unless the server uses VLESS Encryption.
    pub encryption: String,
    pub flow: Option<String>,
    /// Transport: raw/tcp, ws, grpc, httpupgrade, xhttp, ...
    pub network: String,
    /// none/tls/reality
    pub security: String,
    pub sni: Option<String>,
    pub fingerprint: Option<String>,
    pub alpn: Option<String>,
    /// `pcs` — certificate pin, the Xray 26 replacement for `allowInsecure`.
    pub pinned_cert_sha256: Option<String>,
    /// `vcn` — the name to verify the certificate against when it differs
    /// from the SNI sent (the other Xray 26 replacement for `allowInsecure`).
    pub verify_name: Option<String>,
    pub path: Option<String>,
    pub host_header: Option<String>,
    pub service_name: Option<String>,
    /// gRPC `:authority`.
    pub authority: Option<String>,
    /// xhttp `extra` — padding, multiplexing and the like; a server with
    /// custom settings rejects clients that don't send the same.
    pub extra: Option<serde_json::Value>,
    /// `mode` — xhttp mode or `multi` for gRPC.
    pub mode: Option<String>,
    /// `headerType` — only `http` matters (TCP HTTP header obfuscation).
    pub header_type: Option<String>,
    pub public_key: Option<String>,
    pub short_id: Option<String>,
    pub spider_x: Option<String>,
}

/// Full connection parameters for a Hysteria2 client, matching the official
/// client config (https://v2.hysteria.network/docs/getting-started/Client/).
pub struct Hysteria2Params {
    pub host: String,
    /// First (or only) server port — what a latency probe dials.
    pub port: u16,
    /// Port as written in the link: `443`, or a port-hopping spec such as
    /// `20000-50000` / `443,5000-6000` that the client understands natively.
    pub port_spec: String,
    pub password: String,
    pub sni: Option<String>,
    pub insecure: bool,
    pub pin_sha256: Option<String>,
    pub ech: Option<String>,
    /// `(type, password)` — `salamander` or `gecko`.
    pub obfs: Option<(String, String)>,
}

#[allow(clippy::large_enum_variant)] // built once per connect
pub enum ConnectParams {
    Vless(VlessParams),
    Hysteria2(Hysteria2Params),
}

impl ConnectParams {
    pub fn host(&self) -> &str {
        match self {
            ConnectParams::Vless(p) => &p.host,
            ConnectParams::Hysteria2(p) => &p.host,
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            ConnectParams::Vless(p) => p.port,
            ConnectParams::Hysteria2(p) => p.port,
        }
    }
}

/// Re-parses a stored share link into everything a real client core needs to
/// actually dial the server, as opposed to `parse_uri`'s lightweight profile
/// used just for the list UI.
pub fn parse_connect_params(raw: &str) -> Result<ConnectParams, String> {
    let trimmed = raw.trim();
    match scheme_of(trimmed).as_deref() {
        Some("vless") => {
            let url = Url::parse(trimmed).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
            Ok(ConnectParams::Vless(parse_vless_full(&url)?))
        }
        Some("hysteria2" | "hy2") => {
            let (normalized, port_spec) = split_port_spec(trimmed);
            let url =
                Url::parse(&normalized).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
            Ok(ConnectParams::Hysteria2(parse_hysteria2_full(
                &url, port_spec,
            )?))
        }
        Some(other) => Err(format!("Протокол \"{other}\" пока не поддерживается")),
        None => Err("Не удалось разобрать ссылку".to_string()),
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
    let host = host_of(url)?;
    let port = url
        .port()
        .ok_or_else(|| "В ссылке не указан порт".to_string())?;
    let network = match query_get(url, "type")
        .map(|t| t.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("tcp") | Some("raw") => "tcp".to_string(),
        // Renamed upstream; Xray still accepts both, but only one settings key.
        Some("splithttp") => "xhttp".to_string(),
        Some(other) => other.to_string(),
    };
    let security = query_get(url, "security")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "none".to_string());

    Ok(VlessParams {
        host,
        port,
        uuid: decode(url.username()),
        encryption: query_get(url, "encryption").unwrap_or_else(|| "none".to_string()),
        flow: query_get(url, "flow"),
        network,
        security,
        sni: query_get(url, "sni").or_else(|| query_get(url, "peer")),
        fingerprint: query_get(url, "fp"),
        alpn: query_get(url, "alpn"),
        pinned_cert_sha256: query_get(url, "pcs"),
        verify_name: query_get(url, "vcn"),
        path: query_get(url, "path"),
        host_header: query_get(url, "host"),
        service_name: query_get(url, "serviceName"),
        authority: query_get(url, "authority"),
        extra: query_get(url, "extra")
            .and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok())
            .filter(|e| e.is_object()),
        mode: query_get(url, "mode"),
        header_type: query_get(url, "headerType"),
        public_key: query_get(url, "pbk"),
        short_id: query_get(url, "sid"),
        spider_x: query_get(url, "spx"),
    })
}

/// Hysteria2 allows a port-hopping spec in place of a single port
/// (`host:20000-50000`, `host:443,5000-6000`), which `Url` rejects as an
/// invalid port. Swap it for its first port so the rest parses normally and
/// hand the original spec back separately.
fn split_port_spec(raw: &str) -> (String, Option<String>) {
    let unchanged = || (raw.to_string(), None);
    let Some(scheme_end) = raw.find("://") else {
        return unchanged();
    };
    let authority_start = scheme_end + 3;
    let rest = &raw[authority_start..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport_start = authority.rfind('@').map(|i| i + 1).unwrap_or(0);
    let hostport = &authority[hostport_start..];
    let colon = match hostport.rfind(']') {
        Some(bracket) => hostport[bracket..].find(':').map(|i| bracket + i),
        None => hostport.rfind(':'),
    };
    let Some(colon) = colon else {
        return unchanged();
    };
    let spec = &hostport[colon + 1..];
    let is_hopping = spec.contains(['-', ','])
        && spec
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == ',');
    let first: String = spec.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !is_hopping || first.is_empty() {
        return unchanged();
    }
    let spec_start = authority_start + hostport_start + colon + 1;
    let rewritten = format!(
        "{}{}{}",
        &raw[..spec_start],
        first,
        &raw[spec_start + spec.len()..]
    );
    (rewritten, Some(spec.to_string()))
}

fn parse_hysteria2_full(url: &Url, port_spec: Option<String>) -> Result<Hysteria2Params, String> {
    // The URI's userinfo *is* the auth string. With userpass auth it is
    // written as `user:pass@`, and the server expects "user:pass" back.
    let password = match (url.username(), url.password()) {
        ("", None) => return Err("В ссылке Hysteria2 отсутствует пароль".to_string()),
        (user, None) => decode(user),
        (user, Some(pass)) => format!("{}:{}", decode(user), decode(pass)),
    };
    let host = host_of(url)?;
    // The port is optional in Hysteria2 links and defaults to 443.
    let port = url.port().unwrap_or(443);
    // Same truthy spellings as the official client (Go's strconv.ParseBool).
    let insecure = matches!(
        query_get(url, "insecure").as_deref(),
        Some("1" | "t" | "T" | "true" | "TRUE" | "True")
    );
    let obfs_password = query_get(url, "obfs-password");
    let obfs = match query_get(url, "obfs").map(|t| t.to_ascii_lowercase()) {
        // Older links carry just the password; salamander was the only kind.
        None => obfs_password.map(|pw| ("salamander".to_string(), pw)),
        Some(kind) if kind == "salamander" || kind == "gecko" => {
            Some((kind, obfs_password.unwrap_or_default()))
        }
        // The official client treats "plain" as no obfuscation.
        Some(kind) if kind == "plain" => None,
        Some(other) => return Err(format!("Обфускация \"{other}\" не поддерживается")),
    };

    // 3x-ui and v2rayN carry port hopping in `mport` (`20000-30000`, or
    // with `:` as the range separator) rather than in the authority.
    let port_spec = port_spec.or_else(|| {
        query_get(url, "mport")
            .map(|m| m.replace(':', "-"))
            .filter(|m| {
                m.chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == ',')
            })
            .filter(|m| m.chars().any(|c| c.is_ascii_digit()))
    });

    Ok(Hysteria2Params {
        host,
        port,
        port_spec: port_spec.unwrap_or_else(|| port.to_string()),
        password,
        sni: query_get(url, "sni"),
        insecure,
        pin_sha256: query_get(url, "pinSHA256"),
        ech: query_get(url, "ech"),
        obfs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};

    fn vless(uri: &str) -> VlessParams {
        match parse_connect_params(uri).unwrap() {
            ConnectParams::Vless(p) => p,
            _ => panic!("expected vless"),
        }
    }

    fn hysteria2(uri: &str) -> Hysteria2Params {
        match parse_connect_params(uri).unwrap() {
            ConnectParams::Hysteria2(p) => p,
            _ => panic!("expected hysteria2"),
        }
    }

    #[test]
    fn parses_vless_link() {
        let profile = parse_uri(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@nl.example.net:443?type=ws&security=tls#My%20Server",
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
        let profile = parse_uri("hy2://s3cr3t@de.example.net:8443?insecure=1#Berlin").unwrap();
        assert_eq!(profile.protocol, "HYSTERIA2");
        assert_eq!(profile.name, "Berlin");
        assert_eq!(profile.endpoint, "de.example.net:8443");
        assert_eq!(profile.transport, "UDP");
    }

    #[test]
    fn falls_back_to_host_when_no_remark() {
        let profile = parse_uri("vless://uuid@1.2.3.4:443").unwrap();
        assert_eq!(profile.name, "VLESS 1.2.3.4");
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert!(parse_uri("ss://aes-256-gcm@1.2.3.4:8080#x").is_err());
    }

    #[test]
    fn rejects_vless_without_uuid() {
        assert!(parse_uri("vless://@nl.example.net:443").is_err());
    }

    #[test]
    fn rejects_vless_missing_port() {
        assert!(parse_uri("vless://uuid@nl.example.net").is_err());
    }

    #[test]
    fn ids_are_unique_even_when_minted_back_to_back() {
        let a = parse_uri("vless://uuid@a.example.net:443").unwrap();
        let b = parse_uri("vless://uuid@a.example.net:443").unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert!(parse_uri("VLESS://uuid@a.example.net:443#A").is_ok());
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
        assert_eq!(parse_subscription_body(body).len(), 2);
    }

    #[test]
    fn parses_plain_subscription_body_with_bom_and_crlf() {
        let body = "\u{feff}vless://uuid@a.example.net:443#A\r\nhy2://pw@b.example.net:8443#B\r\n";
        assert_eq!(parse_subscription_body(body).len(), 2);
    }

    #[test]
    fn parses_base64_subscription_body_in_every_flavor() {
        // Chosen so the encoding has both padding and URL-unsafe characters.
        let raw =
            "vless://uuid@a.example.net:443?path=%2F%3F%3E#A\nhy2://pw@b.example.net:8443#B??";
        for encoded in [
            STANDARD.encode(raw),
            STANDARD_NO_PAD.encode(raw),
            URL_SAFE_NO_PAD.encode(raw),
        ] {
            assert_eq!(parse_subscription_body(&encoded).len(), 2, "{encoded}");
        }
    }

    #[test]
    fn parses_base64_subscription_wrapped_over_lines() {
        let encoded =
            STANDARD.encode("vless://uuid@a.example.net:443#A\nhy2://pw@b.example.net:8443#B");
        let wrapped: String = encoded
            .as_bytes()
            .chunks(20)
            .map(|c| format!("{}\n", std::str::from_utf8(c).unwrap()))
            .collect();
        assert_eq!(parse_subscription_body(&wrapped).len(), 2);
    }

    #[test]
    fn parses_full_vless_reality_params() {
        let params = vless(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@nl.example.net:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=example.com&fp=chrome&pbk=abc123&sid=de&spx=%2F&type=tcp#Reality",
        );
        assert_eq!(params.host, "nl.example.net");
        assert_eq!(params.port, 443);
        assert_eq!(params.uuid, "b831381d-6324-4d53-ad4f-8cda48b30811");
        assert_eq!(params.flow.as_deref(), Some("xtls-rprx-vision"));
        assert_eq!(params.security, "reality");
        assert_eq!(params.sni.as_deref(), Some("example.com"));
        assert_eq!(params.public_key.as_deref(), Some("abc123"));
        assert_eq!(params.short_id.as_deref(), Some("de"));
        assert_eq!(params.spider_x.as_deref(), Some("/"));
        assert_eq!(params.encryption, "none");
    }

    #[test]
    fn parses_full_vless_ws_tls_params() {
        let params = vless(
            "vless://uuid@nl.example.net:443?type=ws&security=tls&path=%2Fws&host=cdn.example.com&alpn=h2%2Chttp%2F1.1#WS",
        );
        assert_eq!(params.network, "ws");
        assert_eq!(params.security, "tls");
        assert_eq!(params.path.as_deref(), Some("/ws"));
        assert_eq!(params.host_header.as_deref(), Some("cdn.example.com"));
        assert_eq!(params.alpn.as_deref(), Some("h2,http/1.1"));
    }

    #[test]
    fn vless_raw_and_splithttp_are_normalized() {
        assert_eq!(
            vless("vless://uuid@a.example.net:443?type=raw").network,
            "tcp"
        );
        assert_eq!(
            vless("vless://uuid@a.example.net:443?type=splithttp").network,
            "xhttp"
        );
        assert_eq!(vless("vless://uuid@a.example.net:443").network, "tcp");
    }

    #[test]
    fn ipv6_hosts_come_back_without_brackets() {
        let params = vless("vless://uuid@[2001:db8::1]:443");
        assert_eq!(params.host, "2001:db8::1");
        let profile = parse_uri("vless://uuid@[2001:db8::1]:443").unwrap();
        assert_eq!(profile.endpoint, "[2001:db8::1]:443");
    }

    #[test]
    fn parses_full_hysteria2_params() {
        let params = hysteria2(
            "hysteria2://s3cr3t@de.example.net:8443?insecure=1&sni=example.com&obfs=salamander&obfs-password=hunter2&pinSHA256=AB%3ACD#Berlin",
        );
        assert_eq!(params.host, "de.example.net");
        assert_eq!(params.port, 8443);
        assert_eq!(params.port_spec, "8443");
        assert_eq!(params.password, "s3cr3t");
        assert!(params.insecure);
        assert_eq!(params.sni.as_deref(), Some("example.com"));
        assert_eq!(
            params.obfs,
            Some(("salamander".to_string(), "hunter2".to_string()))
        );
        assert_eq!(params.pin_sha256.as_deref(), Some("AB:CD"));
    }

    #[test]
    fn hysteria2_matches_official_client_quirks() {
        // Go's ParseBool spellings.
        assert!(hysteria2("hy2://pw@h.example.net:443?insecure=T").insecure);
        assert!(!hysteria2("hy2://pw@h.example.net:443?insecure=0").insecure);
        // Empty user + password: the official parser sends ":password".
        assert_eq!(
            hysteria2("hy2://:secret@h.example.net:443").password,
            ":secret"
        );
        let gecko = hysteria2("hy2://pw@h.example.net:443?obfs=gecko&obfs-password=g&ech=AAAA");
        assert_eq!(gecko.obfs, Some(("gecko".to_string(), "g".to_string())));
        assert_eq!(gecko.ech.as_deref(), Some("AAAA"));
    }

    #[test]
    fn hysteria2_userpass_auth_keeps_both_halves() {
        assert_eq!(
            hysteria2("hysteria2://user:pa%40ss@h.example.net:443").password,
            "user:pa@ss"
        );
    }

    #[test]
    fn hysteria2_port_defaults_to_443() {
        let params = hysteria2("hysteria2://pw@h.example.net/?sni=x");
        assert_eq!(params.port, 443);
        let profile = parse_uri("hysteria2://pw@h.example.net/?sni=x#A").unwrap();
        assert_eq!(profile.endpoint, "h.example.net:443");
    }

    #[test]
    fn hysteria2_port_hopping_spec_is_preserved() {
        let params = hysteria2("hysteria2://pw@h.example.net:20000-50000/?sni=x#Hop");
        assert_eq!(params.port, 20000);
        assert_eq!(params.port_spec, "20000-50000");
        let params = hysteria2("hy2://pw@[2001:db8::1]:443,5000-6000?insecure=1");
        assert_eq!(params.host, "2001:db8::1");
        assert_eq!(params.port, 443);
        assert_eq!(params.port_spec, "443,5000-6000");
        let profile = parse_uri("hysteria2://pw@h.example.net:20000-50000#Hop").unwrap();
        assert_eq!(profile.endpoint, "h.example.net:20000-50000");
        assert_eq!(profile.name, "Hop");
    }

    #[test]
    fn hysteria2_panel_quirks() {
        let plain = hysteria2("hy2://pw@h.example.net:443?obfs=plain");
        assert!(plain.obfs.is_none());
        let hop = hysteria2("hy2://pw@h.example.net:443?mport=20000:30000#M");
        assert_eq!(hop.port, 443);
        assert_eq!(hop.port_spec, "20000-30000");
        let profile = parse_uri("hy2://pw@h.example.net:443?mport=20000-30000#M").unwrap();
        assert_eq!(profile.endpoint, "h.example.net:20000-30000");
    }

    #[test]
    fn vless_panel_params_are_kept() {
        let p = vless(
            "vless://uuid@104.16.1.1:443?type=xhttp&security=tls&sni=real.example.com&vcn=cert.example.com&extra=%7B%22xPaddingBytes%22%3A%222000-3000%22%7D#X",
        );
        assert_eq!(p.verify_name.as_deref(), Some("cert.example.com"));
        assert_eq!(p.extra.unwrap()["xPaddingBytes"], "2000-3000");
        let g = vless("vless://uuid@1.2.3.4:443?type=grpc&serviceName=s&authority=a.example.com");
        assert_eq!(g.authority.as_deref(), Some("a.example.com"));
        // Garbage `extra` is ignored rather than breaking the import.
        assert!(vless("vless://uuid@1.2.3.4:443?type=xhttp&extra=nope")
            .extra
            .is_none());
    }

    #[test]
    fn group_names_skip_generic_segments_and_tokens() {
        let name = |u: &str| group_name_from_url(&Url::parse(u).unwrap());
        assert_eq!(
            name("https://provA.com/api/v1/client/subscribe?token=a"),
            "provA.com".to_ascii_lowercase()
        );
        assert_eq!(
            name("https://panel.example.net/sub/Zx8kQ2m9LpR4tV7wYb3N/v2ray"),
            "panel.example.net"
        );
        assert_eq!(
            name("https://panel.example.net/sub/Zx8kQ2m9LpR4tV7wYb3N"),
            "panel.example.net"
        );
        assert_eq!(
            name("https://raw.example.com/me/repo/main/work-servers.txt"),
            "work-servers"
        );
    }

    #[test]
    fn profile_title_header_is_decoded() {
        assert_eq!(decode_profile_title("My VPN").as_deref(), Some("My VPN"));
        let b64 = STANDARD.encode("Мой VPN");
        assert_eq!(
            decode_profile_title(&format!("base64:{b64}")).as_deref(),
            Some("Мой VPN")
        );
        assert_eq!(decode_profile_title("   "), None);
    }

    #[test]
    fn hysteria2_rejects_unknown_obfs() {
        assert!(parse_connect_params("hysteria2://pw@h.example.net:443?obfs=gfw").is_err());
    }
}
