//! 服务启动装配模块。
//!
//! 装配流程：加载配置 → 依环境变量 / 配置 / 交互选择平台 → 构建 HTTP 客户端与平台客户端
//! （`OuyiClient::V7` / `V8`）→ 拉取模型目录填充 `ModelMapper`（失败时进入 JWT 交互修复，
//! 用户跳过则退回静态 fallback 模型表）→ 组装 [`AppState`]（限流器、平台客户端、会话缓存、
//! 统计与账户存储等共享状态）→ 合并模型 / 推理路由与管理后台、静态资源路由，附加 CORS 与
//! tracing 中间件 → 启动后台模型刷新任务 → 监听端口并等待 Ctrl+C / SIGTERM 优雅停机。

mod adapter;
mod admin;
mod client;
mod concurrency;
mod config;
mod config_store;
mod error;
mod handler;
mod model;
mod platform;
mod responses_state;
mod router;
mod tool_runtime;

use std::io::{self, BufRead, IsTerminal};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use dashmap::DashMap;
use tokio::signal;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use admin::AccountStore;
use admin::auth::AdminStore;
use admin::stats::StatsCollector;
use client::OuyiClient;
use client::http::OuyiHttpClient;
use client::ouyi_v7::Ouyi7HttpClient;
use concurrency::ConcurrencyLimiter;
use config::{AppConfig, update_ouyi_jwt_token, update_ouyi_v7_jwt_token};
use error::AppError;
use model::catalog::ModelCatalog;
use model::mapper::ModelMapper;
use platform::{OuyiPlatform, PlatformStatus, select_platform_from_config};
use tool_runtime::ToolLedger;

/// 工具调用会话缓存条目：tool_use 响应后暂存会话 ID，等待 tool_result 回传时复用。
pub struct CachedToolSession {
    /// 上游平台分配的会话 ID，tool_result 续轮时复用。
    pub session_id: i64,
    /// 缓存写入时刻，用于过期判定。
    pub created_at: std::time::Instant,
    /// 是否已被某次续轮领取；原子布尔供并发续轮判定领取归属。
    pub claimed: Arc<std::sync::atomic::AtomicBool>,
}

/// 全局共享应用状态，由 `main` 装配后经 `Arc` 注入 axum 路由与后台接口。
pub struct AppState {
    /// 推理请求全局并发限流器，上限取自 `limits.max_concurrent_requests`。
    pub concurrency_limiter: ConcurrencyLimiter,
    /// 可变配置快照；管理后台更新配置时整份替换。
    pub config: RwLock<AppConfig>,
    /// 共享的 HTTP 客户端，超时取自 `limits.request_timeout_secs`。
    pub http_client: reqwest::Client,
    /// 平台客户端：根据启动时选择的平台动态切换
    pub ouyi_client: RwLock<OuyiClient>,
    /// 进行中的上游会话 ID 占位表：请求开始时写入、结束时移除，用于活跃会话清点。
    pub active_sessions: DashMap<i64, ()>,
    /// Anthropic tool_use 会话缓存，key 为对话标识
    pub tool_sessions: DashMap<String, CachedToolSession>,
    /// 工具运行时台账：记录进行中的工具调用、长任务与 tool_use 索引。
    pub tool_ledger: ToolLedger,
    /// Responses 协议按 ID 续轮的会话历史缓存。
    pub responses_state: responses_state::ResponsesState,
    /// 模型目录映射：外部模型名 → 平台模型名（含短名别名），后台刷新任务更新。
    pub model_mapper: ModelMapper,
    /// 管理后台认证存储（JWT 密钥与密码哈希），由后台登录接口读写。
    pub(crate) admin_store: RwLock<AdminStore>,
    /// 上游账号池存储：轮询提供请求用的 JWT Token，并供管理后台查询与增删账号。
    pub account_store: AccountStore,
    /// 请求与用量统计收集器。
    pub stats: StatsCollector,
    /// 当前生效平台，平台切换接口可写。
    pub platform: RwLock<OuyiPlatform>,
    /// 平台运行状态（模型数、Token 有效性等），供状态接口展示。
    pub platform_status: RwLock<PlatformStatus>,
}

/// 服务入口：按模块级文档描述的装配流程完成初始化并阻塞运行 HTTP 服务。
/// 副作用：初始化日志、读配置文件、向上游请求模型列表、必要时进入终端交互修复并写回配置、
/// 启动后台刷新任务、绑定端口监听。
/// 返回 `Err(AppError::Config)`：配置加载失败、监听地址非法或端口绑定失败；
/// `Err(AppError::Internal)`：HTTP 服务运行期 IO 错误。
#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let config = AppConfig::load()
        .map_err(|e: Box<dyn std::error::Error>| AppError::Config(e.to_string()))?;

    // 平台选择
    let selected_platform =
        select_platform_from_config(config.platform.as_ref(), io::stdin().is_terminal())
            .map_err(AppError::Config)?;

    tracing::info!("已选择平台: {}", selected_platform.label());

    if config.ouyi.jwt_token.is_empty() {
        tracing::warn!("JWT token 未配置，请设置 config.toml 中的 ouyi.jwt_token");
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            config.limits.request_timeout_secs,
        ))
        .build()
        .map_err(|e: reqwest::Error| AppError::Config(e.to_string()))?;

    // 根据平台创建对应客户端
    let ouyi_client = build_ouyi_client(&http_client, &config, selected_platform);

    // 尝试初始化平台（加载模型）
    let init_result = initialize_platform(&http_client, &config, selected_platform).await;

    let (config, ouyi_client, model_mapper, platform_status) = match init_result {
        Ok((client, mapper, status)) => (config, client, mapper, status),
        Err(e) => {
            tracing::warn!("平台初始化失败: {}", e);
            tracing::warn!("检测到 JWT 无效或模型列表为空");

            let config_path = PathBuf::from(AppConfig::config_path());
            if let Some((_new_token, new_config, new_client, new_mapper)) =
                tokio::task::block_in_place(|| {
                    interactive_jwt_fix(&http_client, &config, &config_path, selected_platform)
                })
            {
                let base_url = match &new_client {
                    OuyiClient::V7(v7) => v7.base_url().to_string(),
                    OuyiClient::V8(_) => new_config.ouyi.base_url.clone(),
                };
                let mut status = PlatformStatus::new(selected_platform, base_url);
                status.token_valid = true;
                status.model_count = new_mapper.model_count();
                (new_config, new_client, new_mapper, status)
            } else {
                // 用户跳过交互，使用 fallback
                let mapper = ModelMapper::new();
                mapper.load_fallback();
                tracing::warn!("使用静态 fallback 模型列表启动");
                let base_url = match &ouyi_client {
                    OuyiClient::V7(v7) => v7.base_url().to_string(),
                    OuyiClient::V8(_) => config.ouyi.base_url.clone(),
                };
                (
                    config,
                    ouyi_client,
                    mapper,
                    PlatformStatus::new(selected_platform, base_url),
                )
            }
        }
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e: std::net::AddrParseError| AppError::Config(e.to_string()))?;

    let model_mapper_clone = model_mapper.clone();

    // Admin store initialization
    let jwt_secret = config
        .admin
        .as_ref()
        .and_then(|a| a.jwt_secret.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let password_hash = config
        .admin
        .as_ref()
        .and_then(|a| a.password_hash.clone())
        .unwrap_or_default();

    let admin_store = AdminStore::new(jwt_secret.clone(), password_hash.clone());
    let account_store = AccountStore::new();
    let stats = StatsCollector::new();

    let state = Arc::new(AppState {
        concurrency_limiter: ConcurrencyLimiter::new(config.limits.max_concurrent_requests),
        config: RwLock::new(config.clone()),
        http_client,
        ouyi_client: RwLock::new(ouyi_client),
        active_sessions: DashMap::new(),
        tool_sessions: DashMap::new(),
        tool_ledger: ToolLedger::new(),
        responses_state: responses_state::ResponsesState::default(),
        model_mapper,
        admin_store: RwLock::new(admin_store),
        account_store,
        stats,
        platform: RwLock::new(selected_platform),
        platform_status: RwLock::new(platform_status),
    });

    // 后台模型刷新任务
    let refresh_state = state.clone();
    let refresh_interval = match selected_platform {
        OuyiPlatform::Ouyi7 => config
            .ouyi_v7
            .as_ref()
            .map(|v| v.model_refresh_interval_secs)
            .unwrap_or(1800),
        OuyiPlatform::Ouyi8 => config.ouyi.model_refresh_interval_secs,
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(refresh_interval));
        loop {
            interval.tick().await;
            let client = refresh_state.ouyi_client.read().await;
            match load_platform_catalog(&client).await {
                Ok(catalog) => {
                    // V7：catalog 加载后同步模型路由表给 client（customAIModels/aiModels 的路由决策）
                    if let crate::client::OuyiClient::V7(v7) = &*client {
                        let table = crate::client::ouyi_v7::build_routing_table_from_catalog(&catalog);
                        v7.update_routing(table);
                    }
                    refresh_state.model_mapper.load_from_catalog(&catalog);
                    let count = refresh_state.model_mapper.model_count();
                    let mut status = refresh_state.platform_status.write().await;
                    status.model_count = count;
                    status.token_valid = true;
                    tracing::info!("模型列表已刷新: {} 个模型", count);
                }
                Err(e) => {
                    tracing::warn!("模型列表刷新失败: {}, 10分钟后重试", e);
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                }
            }
        }
    });

    let admin_router = admin::admin_routes(state.clone());
    let static_router = admin::static_file_router();

    let app = router::app(state.concurrency_limiter.clone())
        .merge(admin_router)
        .merge(static_router)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e: std::io::Error| AppError::Config(e.to_string()))?;

    tracing::info!("欧亿AI反代服务启动于 http://{}", addr);
    tracing::info!(
        "当前平台: {}, 可用模型: {} 个, 短名映射: {} 个",
        selected_platform.label(),
        model_mapper_clone.model_count(),
        model_mapper_clone.get_all_short_names().len()
    );

    let service = app.into_make_service_with_connect_info::<SocketAddr>();

    axum::serve(listener, service)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e: std::io::Error| AppError::Internal(e.to_string()))?;

    tracing::info!("服务已停止，清理活跃会话...");
    Ok(())
}

/// 等待停机信号：Ctrl+C 或 Unix 下的 SIGTERM，收到后记录日志并返回以触发优雅停机。
/// 非 Unix 平台没有对应信号源，该分支永不完成。
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to listen for ctrl+c");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("收到 Ctrl+C 信号"),
        _ = terminate => tracing::info!("收到终止信号"),
    }
}

/// 根据平台构造对应的平台客户端。
/// - 7.0：读取 `[ouyi_v7]` 段（缺字段时使用内置默认值），空 Token 视为未配置；
/// - 8.0：读取 `[ouyi]` 段。
/// 返回封装好基础地址与凭证的 [`OuyiClient`]，不发起网络请求。
pub(crate) fn build_ouyi_client(
    http_client: &reqwest::Client,
    config: &AppConfig,
    platform: OuyiPlatform,
) -> OuyiClient {
    match platform {
        OuyiPlatform::Ouyi7 => {
            let v7_config = config.ouyi_v7.as_ref();
            let base_url = v7_config
                .map(|c| c.base_url.clone())
                .unwrap_or_else(|| "https://api-8.rcouyi.com".to_string());
            let chatapi_prefix = v7_config
                .map(|c| c.chatapi_prefix.clone())
                .unwrap_or_else(|| "/chatapi".to_string());
            let jwt_token = v7_config
                .as_ref()
                .filter(|c| !c.jwt_token.is_empty())
                .map(|c| c.jwt_token.clone())
                .unwrap_or_default();
            OuyiClient::V7(Ouyi7HttpClient::new(
                http_client.clone(),
                base_url,
                chatapi_prefix,
                jwt_token,
            ))
        }
        OuyiPlatform::Ouyi8 => OuyiClient::V8(OuyiHttpClient::new(
            http_client.clone(),
            config.ouyi.base_url.clone(),
            config.ouyi.jwt_token.clone(),
            config.ouyi.app_version.clone(),
        )),
    }
}

/// 与 [`build_ouyi_client`] 相同，但以调用方传入的 `jwt_token` 覆盖配置中的凭证，
/// 供 JWT 修复流程构造临时客户端验证候选 Token。
fn build_ouyi_client_with_token(
    http_client: &reqwest::Client,
    config: &AppConfig,
    platform: OuyiPlatform,
    jwt_token: String,
) -> OuyiClient {
    match platform {
        OuyiPlatform::Ouyi7 => {
            let v7_config = config.ouyi_v7.as_ref();
            let base_url = v7_config
                .map(|c| c.base_url.clone())
                .unwrap_or_else(|| "https://api-8.rcouyi.com".to_string());
            let chatapi_prefix = v7_config
                .map(|c| c.chatapi_prefix.clone())
                .unwrap_or_else(|| "/chatapi".to_string());
            OuyiClient::V7(Ouyi7HttpClient::new(
                http_client.clone(),
                base_url,
                chatapi_prefix,
                jwt_token,
            ))
        }
        OuyiPlatform::Ouyi8 => OuyiClient::V8(OuyiHttpClient::new(
            http_client.clone(),
            config.ouyi.base_url.clone(),
            jwt_token,
            config.ouyi.app_version.clone(),
        )),
    }
}

/// 从平台客户端拉取模型目录。
/// - 7.0：串行请求成员信息与模型枚举接口后合成目录，模型列表为空时报错；
/// - 8.0：请求模板接口并解析为目录。
/// 副作用：向上游发起网络请求。
/// 返回 `Err(AppError::Upstream)`：任一上游请求失败或模型列表为空。
pub(crate) async fn load_platform_catalog(client: &OuyiClient) -> Result<ModelCatalog, AppError> {
    match client {
        OuyiClient::V7(v7) => {
            let member_info = v7.fetch_member_info().await?;
            let enum_items = v7.fetch_model_enum().await?;
            let catalog = ModelCatalog::from_ouyi_v7(&member_info, &enum_items);
            if catalog.models.is_empty() {
                return Err(AppError::Upstream("模型列表为空".to_string()));
            }
            Ok(catalog)
        }
        OuyiClient::V8(v8) => {
            let tmpl = v8.fetch_tmpl().await?;
            Ok(ModelCatalog::from_tmpl(&tmpl))
        }
    }
}

/// 平台初始化：构造客户端、拉取模型目录填充 [`ModelMapper`]、生成 [`PlatformStatus`]。
/// 副作用：向上游发起网络请求、写 tracing 日志。
/// 返回成功组装的三元组（客户端、映射器、状态）；返回 `Err(AppError::Upstream)`：
/// 模型列表拉取失败或为空，其余错误沿 [`load_platform_catalog`] 透传。
async fn initialize_platform(
    http_client: &reqwest::Client,
    config: &AppConfig,
    platform: OuyiPlatform,
) -> Result<(OuyiClient, ModelMapper, PlatformStatus), AppError> {
    let ouyi_client = build_ouyi_client(http_client, config, platform);
    let base_url = match &ouyi_client {
        OuyiClient::V7(v7) => v7.base_url().to_string(),
        OuyiClient::V8(_) => config.ouyi.base_url.clone(),
    };
    let platform_status = PlatformStatus::new(platform, base_url);
    let model_mapper = ModelMapper::new();

    let catalog = load_platform_catalog(&ouyi_client).await?;
    model_mapper.load_from_catalog(&catalog);
    let mut ps = platform_status;
    ps.model_count = model_mapper.model_count();
    if ps.model_count == 0 {
        return Err(AppError::Upstream("模型列表为空".to_string()));
    }
    ps.token_valid = true;
    tracing::info!("模型列表加载成功: {} 个模型", model_mapper.model_count());
    Ok((ouyi_client, model_mapper, ps))
}

/// JWT 修复收尾：按平台取配置中的 Token 重建客户端，并把已验证通过的模型目录
/// 载入新的 [`ModelMapper`]，不再重复请求上游。
/// 返回 `Err(AppError::Upstream)`：传入的模型目录为空。
fn finalize_jwt_fix_with_catalog(
    http_client: &reqwest::Client,
    config: &AppConfig,
    platform: OuyiPlatform,
    catalog: ModelCatalog,
) -> Result<(OuyiClient, ModelMapper), AppError> {
    if catalog.models.is_empty() {
        return Err(AppError::Upstream("模型列表为空".to_string()));
    }

    let jwt_token = match platform {
        OuyiPlatform::Ouyi7 => config
            .ouyi_v7
            .as_ref()
            .map(|c| c.jwt_token.clone())
            .unwrap_or_default(),
        OuyiPlatform::Ouyi8 => config.ouyi.jwt_token.clone(),
    };

    let client = build_ouyi_client_with_token(http_client, config, platform, jwt_token);
    let mapper = ModelMapper::new();
    mapper.load_from_catalog(&catalog);

    Ok((client, mapper))
}

/// JWT 交互自修复：平台初始化失败时在终端提示输入新 Token。
/// 流程：读取输入（空输入直接跳过并返回 `None`）→ 构造临时客户端验证 Token →
/// 拉取模型目录（为空或失败则提示后继续等待输入）→ 经 `update_ouyi_jwt_token` /
/// `update_ouyi_v7_jwt_token` 把新 Token 原子写回配置文件 → 重新加载配置并重建客户端与映射器。
/// 非 TTY 环境直接返回 `None`；配置写回失败也返回 `None`。
/// 副作用：读标准输入（阻塞等待用户输入）、向上游发起验证请求、覆盖写配置文件中的平台 Token。
/// 返回 `Some((token, 新配置, 新客户端, 新映射器))` 表示修复成功。
fn interactive_jwt_fix(
    http_client: &reqwest::Client,
    config: &AppConfig,
    config_path: &std::path::Path,
    platform: OuyiPlatform,
) -> Option<(String, AppConfig, OuyiClient, ModelMapper)> {
    if !io::stdin().is_terminal() {
        tracing::warn!("当前运行环境不支持交互输入，跳过 JWT 自修复");
        return None;
    }

    println!("请输入新的 Ouyi JWT Token 后按回车（直接回车跳过）：");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        println!("> ");
        let line = lines.next()?.ok()?;
        let token = line.trim();

        if token.is_empty() {
            tracing::info!("跳过 JWT 自修复，使用原配置启动");
            return None;
        }

        // 根据平台构造临时客户端验证新 Token
        let rt = tokio::runtime::Handle::current();

        let temp_client =
            build_ouyi_client_with_token(http_client, config, platform, token.to_string());

        // 验证 Token
        let token_valid = match &temp_client {
            OuyiClient::V7(v7) => rt.block_on(v7.verify_token()).ok().unwrap_or(false),
            OuyiClient::V8(v8) => rt.block_on(v8.verify_token()).ok().unwrap_or(false),
        };

        if !token_valid {
            println!("Token 验证失败，请检查输入是否正确");
            continue;
        }

        // 验证并获取模型列表
        let verified_catalog = match &temp_client {
            OuyiClient::V7(v7) => {
                let rt = tokio::runtime::Handle::current();
                let member_result = rt.block_on(v7.fetch_member_info());
                let enum_result = rt.block_on(v7.fetch_model_enum());
                match (member_result, enum_result) {
                    (Ok(member_info), Ok(enum_items)) => {
                        let catalog = ModelCatalog::from_ouyi_v7(&member_info, &enum_items);
                        if catalog.models.is_empty() {
                            println!("Token 验证通过但模型列表为空，请检查 Token 权限");
                            continue;
                        }
                        tracing::info!("7.0 Token 验证通过，模型数: {}", catalog.models.len());
                        catalog
                    }
                    _ => {
                        println!("获取模型列表失败，请检查 Token 权限");
                        continue;
                    }
                }
            }
            OuyiClient::V8(v8) => {
                let tmpl_result = rt.block_on(v8.fetch_tmpl());
                match tmpl_result {
                    Ok(ref resp) => {
                        let catalog = ModelCatalog::from_tmpl(resp);
                        if catalog.models.is_empty() {
                            println!("Token 验证通过但模型列表为空，请检查 Token 权限");
                            continue;
                        }
                        catalog
                    }
                    Err(e) => {
                        println!("获取模型列表失败: {}", e);
                        continue;
                    }
                }
            }
        };

        // 写回配置文件
        let write_result = match platform {
            OuyiPlatform::Ouyi7 => update_ouyi_v7_jwt_token(config_path, token),
            OuyiPlatform::Ouyi8 => update_ouyi_jwt_token(config_path, token),
        };
        if let Err(e) = write_result {
            tracing::error!("配置写回失败: {}", e);
            println!("配置写回失败，请手动更新 config.toml");
            return None;
        }

        // 重新加载配置
        let new_config = AppConfig::load()
            .map_err(|e| {
                tracing::error!("重新加载配置失败: {}", e);
            })
            .ok()?;

        let (new_client, new_mapper) = match finalize_jwt_fix_with_catalog(
            http_client,
            &new_config,
            platform,
            verified_catalog,
        ) {
            Ok(result) => result,
            Err(e) => {
                println!("加载模型列表失败: {}", e);
                continue;
            }
        };

        tracing::info!("JWT 自修复成功，模型列表: {} 个", new_mapper.model_count());
        println!("JWT Token 已更新并验证通过，服务将继续启动...");

        return Some((token.to_string(), new_config, new_client, new_mapper));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};

    async fn spawn_test_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    async fn member_info_with_custom_model() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "code": 200,
            "result": {
                "groupInfo": {
                    "privileges": [
                        { "customAIModels": ["gpt-4o"] }
                    ]
                }
            }
        }))
    }

    async fn empty_enum_list() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "code": 200,
            "result": []
        }))
    }

    fn test_config_with_v7_base_url(base_url: &str) -> AppConfig {
        toml::from_str(&format!(
            r#"
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://api-8.rcouyi.com"
jwt_token = "v8-token"
app_version = "3.0.3"
model_refresh_interval_secs = 3600

[limits]
request_timeout_secs = 30
no_data_timeout_secs = 30
max_response_bytes = 1048576
max_messages_bytes = 1048576

[ouyi_v7]
base_url = "{base_url}"
chatapi_prefix = "/chatapi"
jwt_token = "old-token"
default_model = "ouyi-chat"
model_refresh_interval_secs = 1800
max_history_messages = 20
simulated_stream_chunk_chars = 80
"#
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn test_load_platform_catalog_keeps_member_models_when_enum_is_empty() {
        let app = Router::new()
            .route(
                "/chatapi/auth/memberInfo",
                get(member_info_with_custom_model),
            )
            .route("/api/sysEnum/enumDataList", get(empty_enum_list));
        let base_url = spawn_test_server(app).await;
        let client = OuyiClient::V7(Ouyi7HttpClient::new(
            reqwest::Client::new(),
            base_url,
            "/chatapi".to_string(),
            "test-jwt".to_string(),
        ));

        let catalog = load_platform_catalog(&client).await.unwrap();

        assert_eq!(catalog.platform, OuyiPlatform::Ouyi7);
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].id, "gpt-4o");
    }

    #[test]
    fn test_finalize_jwt_fix_reuses_verified_catalog_for_ouyi7() {
        let config = test_config_with_v7_base_url("http://127.0.0.1:9");
        let catalog = ModelCatalog {
            platform: OuyiPlatform::Ouyi7,
            models: vec![model::catalog::ModelCatalogItem {
                id: "gpt-4o".to_string(),
                label: "gpt-4o".to_string(),
                provider_key: "openai".to_string(),
                provider_name: "OpenAI".to_string(),
                capabilities: model::catalog::default_ouyi_v7_capabilities(),
                metadata: None,
            }],
        };

        let (_client, mapper) = finalize_jwt_fix_with_catalog(
            &reqwest::Client::new(),
            &config,
            OuyiPlatform::Ouyi7,
            catalog,
        )
        .unwrap();

        assert_eq!(mapper.model_count(), 1);
        assert_eq!(mapper.to_ouyi("gpt-4o"), "gpt-4o");
    }

    #[tokio::test]
    #[ignore = "requires local config.toml credentials and external Ouyi network"]
    async fn real_configured_ouyi_jwt_token_probe_without_printing_secret() {
        let config = AppConfig::load().expect("config should load");
        let selected_platform = select_platform_from_config(config.platform.as_ref(), false)
            .expect("platform selection should not prompt in tests");
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("http client should build");

        match selected_platform {
            OuyiPlatform::Ouyi8 => {
                assert!(
                    !config.ouyi.jwt_token.trim().is_empty(),
                    "configured Ouyi 8.0 JWT token is empty"
                );
                let client = OuyiHttpClient::new(
                    http_client,
                    config.ouyi.base_url,
                    config.ouyi.jwt_token,
                    config.ouyi.app_version,
                );
                let valid = client
                    .verify_token()
                    .await
                    .expect("Ouyi 8.0 token probe request should complete");
                assert!(valid, "configured Ouyi 8.0 JWT token was rejected");
            }
            OuyiPlatform::Ouyi7 => {
                let v7_config = config
                    .ouyi_v7
                    .expect("configured platform is Ouyi 7.0 but [ouyi_v7] is missing");
                assert!(
                    !v7_config.jwt_token.trim().is_empty(),
                    "configured Ouyi 7.0 JWT token is empty"
                );
                let client = Ouyi7HttpClient::new(
                    http_client,
                    v7_config.base_url,
                    v7_config.chatapi_prefix,
                    v7_config.jwt_token,
                );
                let valid = client
                    .verify_token()
                    .await
                    .expect("Ouyi 7.0 token probe request should complete");
                assert!(valid, "configured Ouyi 7.0 JWT token was rejected");
            }
        }
    }
}
