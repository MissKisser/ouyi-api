//! OpenAI 风格模型列表端点：`GET /v1/models` 与 `GET /openai/v1/models`。
//! 基于模型映射表生成当前可用的短模型名清单。

use axum::extract::State;
use axum::response::IntoResponse;
use std::sync::Arc;

use crate::AppState;
use crate::model::openai::{ModelInfo, ModelListResponse};

/// 列出当前可用的模型（OpenAI `GET /v1/models` 协议）。
/// 从 `model_mapper` 取全部短模型名，逐个解析其归属平台作为 `owned_by`，
/// `created` 取当前 Unix 时间戳。
/// 无请求体入参；成功返回 `ModelListResponse`（`object: "list"` + `data` 数组），
/// 模型名解析失败时 `owned_by` 回退为 `"unknown"`。
/// 该端点只读本地映射表，不发起上游请求。
pub async fn list_models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let short_names = state.model_mapper.get_all_short_names();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let models: Vec<ModelInfo> = short_names
        .into_iter()
        .map(|id| {
            let owned_by = state
                .model_mapper
                .get_provider_name(&state.model_mapper.to_ouyi(&id))
                .unwrap_or_else(|| "unknown".to_string());
            ModelInfo {
                id,
                object: "model".to_string(),
                created: now,
                owned_by,
            }
        })
        .collect();

    axum::Json(ModelListResponse {
        object: "list".to_string(),
        data: models,
    })
}
