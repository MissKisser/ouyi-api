pub const TOOL_RUNTIME_HOP_HEADER: &str = "x-ouyi-tool-hop";
pub const TOOL_RUNTIME_HOP_VALUE: &str = "1";

pub fn reject_self_referential_base_url(
    base_url: &str,
    server_host: &str,
    server_port: u16,
) -> Result<(), String> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|error| format!("工具模型 base_url 无效: {error}"))?;
    let host = url.host_str().unwrap_or_default();
    let port = url.port_or_known_default().unwrap_or_default();
    let server_is_wildcard = matches!(server_host, "0.0.0.0" | "::" | "[::]");
    let host_matches =
        host.eq_ignore_ascii_case(server_host) || (server_is_wildcard && is_loopback_host(host));
    if host_matches && port == server_port {
        return Err("工具模型地址指向当前反代监听地址".to_string());
    }
    Ok(())
}

pub fn reject_public_origin(base_url: &str, public_origin: Option<&str>) -> Result<(), String> {
    let Some(public_origin) = public_origin.filter(|origin| !origin.trim().is_empty()) else {
        return Ok(());
    };
    let base = reqwest::Url::parse(base_url)
        .map_err(|error| format!("工具模型 base_url 无效: {error}"))?;
    let public = reqwest::Url::parse(public_origin)
        .map_err(|error| format!("OUYI_PUBLIC_ORIGIN 无效: {error}"))?;
    if same_origin(&base, &public) {
        return Err("工具模型地址指向当前反代公开域名".to_string());
    }
    Ok(())
}

pub fn validate_compiler_base_url(base_url: &str) -> Result<(), String> {
    let host = std::env::var("SERVER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("SERVER_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(3000);
    reject_self_referential_base_url(base_url, &host, port)?;
    reject_public_origin(
        base_url,
        std::env::var("OUYI_PUBLIC_ORIGIN").ok().as_deref(),
    )
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_same_loopback_listener_but_allows_different_port() {
        assert!(
            reject_self_referential_base_url("http://127.0.0.1:4117/v1", "127.0.0.1", 4117,)
                .is_err()
        );
        assert!(
            reject_self_referential_base_url("http://127.0.0.1:5000/v1", "127.0.0.1", 4117,)
                .is_ok()
        );
    }

    #[test]
    fn rejects_configured_public_origin() {
        assert!(
            reject_public_origin(
                "https://ouyi.viaxv.cn/anthropic",
                Some("https://ouyi.viaxv.cn"),
            )
            .is_err()
        );
    }

    #[test]
    fn exposes_single_hop_header_contract() {
        assert_eq!(TOOL_RUNTIME_HOP_HEADER, "x-ouyi-tool-hop");
        assert_eq!(TOOL_RUNTIME_HOP_VALUE, "1");
    }
}
