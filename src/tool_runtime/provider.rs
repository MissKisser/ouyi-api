//! 工具模型（编译器）提供方的地址安全防护。
//! 防止工具模型的 base_url 指向反代自身（监听地址或公开域名）造成
//! 请求自环；并定义工具调用链路的单跳标记头约定。

/// 工具运行时链路的单跳标记头名称。
pub const TOOL_RUNTIME_HOP_HEADER: &str = "x-ouyi-tool-hop";
/// 单跳标记头的取值。
pub const TOOL_RUNTIME_HOP_VALUE: &str = "1";

/// 拒绝指向当前服务监听地址的工具模型 base_url。
///
/// 主机名与监听主机一致（监听主机为通配地址时，回环主机也视为一致）
/// 且端口相同时视为自环。
///
/// # 参数
/// - `base_url`：工具模型服务地址。
/// - `server_host`：当前服务监听主机。
/// - `server_port`：当前服务监听端口。
///
/// # 返回
/// 非自环返回 `Ok(())`。
///
/// # 错误
/// base_url 无法解析或构成自环时，返回描述性错误字符串。
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

/// 拒绝指向当前服务公开域名的工具模型 base_url。
///
/// # 参数
/// - `base_url`：工具模型服务地址。
/// - `public_origin`：服务公开域名（如 `OUYI_PUBLIC_ORIGIN` 配置）；
///   为 `None` 或空白时直接放行。
///
/// # 返回
/// 未构成同源自环返回 `Ok(())`。
///
/// # 错误
/// 任一 URL 无法解析或与公开域名同源（协议、主机、端口一致）时，
/// 返回描述性错误字符串。
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

/// 综合校验编译器 base_url：禁止指向本服务的监听地址或公开域名。
///
/// 监听主机与端口取自环境变量 `SERVER_HOST` / `SERVER_PORT`
/// （缺省 `127.0.0.1` / 3000），公开域名取自环境变量 `OUYI_PUBLIC_ORIGIN`。
///
/// # 参数
/// - `base_url`：编译器服务地址。
///
/// # 返回
/// 校验通过返回 `Ok(())`。
///
/// # 错误
/// 任一子检查失败时返回描述性错误字符串（见
/// [`reject_self_referential_base_url`] 与 [`reject_public_origin`]）。
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
                "https://self.example.test/anthropic",
                Some("https://self.example.test"),
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
