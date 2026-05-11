use axum::extract::State;
use axum::response::IntoResponse;
use std::sync::Arc;

use crate::model::openai::{ModelInfo, ModelListResponse};
use crate::AppState;

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
