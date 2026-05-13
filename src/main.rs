mod adapter;
mod admin;
mod client;
mod config;
mod error;
mod handler;
mod model;
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
use client::http::OuyiHttpClient;
use config::{AppConfig, update_ouyi_jwt_token};
use error::AppError;
use model::mapper::ModelMapper;
use tool_runtime::ToolLedger;

/// 工具调用会话缓存：tool_use 响应后暂存会话 ID，等待 tool_result 回传时复用
pub struct CachedToolSession {
    pub session_id: i64,
    pub created_at: std::time::Instant,
}

pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub http_client: reqwest::Client,
    pub ouyi_client: RwLock<OuyiHttpClient>,
    pub active_sessions: DashMap<i64, ()>,
    /// Anthropic tool_use 会话缓存，key 为对话标识
    pub tool_sessions: DashMap<String, CachedToolSession>,
    pub tool_ledger: ToolLedger,
    pub model_mapper: ModelMapper,
    pub(crate) admin_store: RwLock<AdminStore>,
    pub account_store: AccountStore,
    pub stats: StatsCollector,
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let config = AppConfig::load()
        .map_err(|e: Box<dyn std::error::Error>| AppError::Config(e.to_string()))?;

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

    // 加载模型列表并检测是否需要 JWT 自修复
    let model_mapper = ModelMapper::new();
    let (tmpl_result, token_valid) = {
        let tmpl = ouyi_client.fetch_tmpl().await;
        let valid = ouyi_client.verify_token().await.ok().unwrap_or(false);
        (tmpl, valid)
    };

    let needs_jwt_fix = match &tmpl_result {
        Ok(tmpl_resp) => {
            let model_count = tmpl_resp.data.as_ref().map(|d| d.models.len()).unwrap_or(0);
            model_count == 0
        }
        Err(e) => {
            let err_str = e.to_string().to_lowercase();
            err_str.contains("鉴权")
                || err_str.contains("auth")
                || err_str.contains("token")
                || err_str.contains("401")
                || err_str.contains("403")
                || err_str.contains("空")
        }
    } || !token_valid;

    let (config, ouyi_client, model_mapper) = if needs_jwt_fix {
        tracing::warn!("检测到 Ouyi JWT 无效或模型列表为空");
        let config_path = PathBuf::from(AppConfig::config_path());

        if let Some((_new_token, new_config, new_client, new_mapper)) =
            tokio::task::block_in_place(|| interactive_jwt_fix(&http_client, &config, &config_path))
        {
            (new_config, new_client, new_mapper)
        } else {
            // 用户跳过交互，使用 fallback
            model_mapper.load_fallback();
            tracing::warn!("使用静态 fallback 模型列表启动");
            (config, ouyi_client, model_mapper)
        }
    } else {
        match tmpl_result {
            Ok(tmpl_resp) => {
                model_mapper.load_from_tmpl(&tmpl_resp);
                tracing::info!("模型列表加载成功: {} 个模型", model_mapper.model_count());
            }
            Err(e) => {
                tracing::warn!("模型列表加载失败: {}, 使用静态 fallback", e);
                model_mapper.load_fallback();
            }
        }
        (config, ouyi_client, model_mapper)
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
        config: RwLock::new(config.clone()),
        http_client,
        ouyi_client: RwLock::new(ouyi_client),
        active_sessions: DashMap::new(),
        tool_sessions: DashMap::new(),
        tool_ledger: ToolLedger::new(),
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

fn interactive_jwt_fix(
    http_client: &reqwest::Client,
    config: &AppConfig,
    config_path: &std::path::Path,
) -> Option<(String, AppConfig, OuyiHttpClient, ModelMapper)> {
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

        // 构造临时客户端验证新 Token
        let temp_client = OuyiHttpClient::new(
            http_client.clone(),
            config.ouyi.base_url.clone(),
            token.to_string(),
            config.ouyi.app_version.clone(),
        );

        // 验证 Token（同步执行异步操作）
        let rt = tokio::runtime::Handle::current();
        let verify_result = rt.block_on(temp_client.verify_token());
        let token_valid = verify_result.ok().unwrap_or(false);

        if !token_valid {
            println!("Token 验证失败，请检查输入是否正确");
            continue;
        }

        // 获取模型列表
        let tmpl_result = rt.block_on(temp_client.fetch_tmpl());
        match tmpl_result {
            Ok(ref resp) => {
                let model_count = resp.data.as_ref().map(|d| d.models.len()).unwrap_or(0);
                if model_count == 0 {
                    println!("Token 验证通过但模型列表为空，请检查 Token 权限");
                    continue;
                }
            }
            Err(e) => {
                println!("获取模型列表失败: {}", e);
                continue;
            }
        }

        // 写回配置文件
        if let Err(e) = update_ouyi_jwt_token(config_path, token) {
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

        // 初始化新客户端和模型映射
        let new_client = OuyiHttpClient::new(
            http_client.clone(),
            new_config.ouyi.base_url.clone(),
            new_config.ouyi.jwt_token.clone(),
            new_config.ouyi.app_version.clone(),
        );

        let new_mapper = ModelMapper::new();
        new_mapper.load_from_tmpl(&tmpl_result.unwrap());

        tracing::info!("JWT 自修复成功，模型列表: {} 个", new_mapper.model_count());
        println!("JWT Token 已更新并验证通过，服务将继续启动...");

        return Some((token.to_string(), new_config, new_client, new_mapper));
    }
}
