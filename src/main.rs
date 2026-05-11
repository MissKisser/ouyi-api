mod adapter;
mod admin;
mod client;
mod config;
mod error;
mod handler;
mod model;
mod router;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use dashmap::DashMap;
use tokio::signal;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use admin::auth::AdminStore;
use admin::AccountStore;
use admin::stats::StatsCollector;
use client::http::OuyiHttpClient;
use config::AppConfig;
use error::AppError;
use model::mapper::ModelMapper;

/// 工具调用会话缓存：tool_use 响应后暂存会话 ID，等待 tool_result 回传时复用
pub struct CachedToolSession {
    pub session_id: i64,
    pub model: String,
    pub created_at: std::time::Instant,
}

pub struct AppState {
    pub config: AppConfig,
    pub http_client: reqwest::Client,
    pub ouyi_client: RwLock<OuyiHttpClient>,
    pub active_sessions: DashMap<i64, ()>,
    /// Anthropic tool_use 会话缓存，key 为对话标识
    pub tool_sessions: DashMap<String, CachedToolSession>,
    pub model_mapper: ModelMapper,
    pub(crate) admin_store: RwLock<AdminStore>,
    pub account_store: AccountStore,
    pub stats: StatsCollector,
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("info".parse().unwrap()),
        )
        .init();

    let config = AppConfig::load().map_err(|e: Box<dyn std::error::Error>| {
        AppError::Config(e.to_string())
    })?;

    if config.ouyi.jwt_token.is_empty() {
        tracing::warn!("JWT token 未配置，请设置 config.toml 中的 ouyi.jwt_token");
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            config.limits.request_timeout_secs,
        ))
        .build()
        .map_err(|e: reqwest::Error| AppError::Config(e.to_string()))?;

    let ouyi_client = OuyiHttpClient::new(
        http_client.clone(),
        config.ouyi.base_url.clone(),
        config.ouyi.jwt_token.clone(),
        config.ouyi.app_version.clone(),
    );

    // 加载模型列表
    let model_mapper = ModelMapper::new();
    match ouyi_client.fetch_tmpl().await {
        Ok(tmpl_resp) => {
            model_mapper.load_from_tmpl(&tmpl_resp);
            tracing::info!("模型列表加载成功: {} 个模型", model_mapper.model_count());
        }
        Err(e) => {
            tracing::warn!("模型列表加载失败: {}, 使用静态fallback", e);
            model_mapper.load_fallback();
        }
    }

    // 验证JWT token
    match ouyi_client.verify_token().await {
        Ok(true) => tracing::info!("JWT token 验证通过"),
        Ok(false) => tracing::warn!("JWT token 验证失败，请检查配置"),
        Err(e) => tracing::warn!("JWT token 验证请求失败: {}", e),
    }

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
        config: config.clone(),
        http_client,
        ouyi_client: RwLock::new(ouyi_client),
        active_sessions: DashMap::new(),
        tool_sessions: DashMap::new(),
        model_mapper,
        admin_store: RwLock::new(admin_store),
        account_store,
        stats,
    });

    // 后台模型刷新任务
    let refresh_state = state.clone();
    let refresh_interval = config.ouyi.model_refresh_interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(refresh_interval));
        loop {
            interval.tick().await;
            let client = refresh_state.ouyi_client.read().await;
            match client.fetch_tmpl().await {
                Ok(tmpl_resp) => {
                    let count = tmpl_resp.data.as_ref().map(|d| d.models.len()).unwrap_or(0);
                    refresh_state.model_mapper.load_from_tmpl(&tmpl_resp);
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

    let app = router::app()
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
        "可用模型: {} 个, 短名映射: {} 个",
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

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl+c");
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
