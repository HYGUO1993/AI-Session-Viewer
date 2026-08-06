use axum::http::StatusCode;
use axum::response::Json;
use session_core::sync_config::{self, ApplyMcpRequest, ConfigSyncManifest};

pub async fn get_manifest() -> Result<Json<ConfigSyncManifest>, (StatusCode, String)> {
    tokio::task::spawn_blocking(sync_config::read_config_sync_manifest)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub async fn apply_mcp(
    Json(request): Json<ApplyMcpRequest>,
) -> Result<Json<()>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || sync_config::apply_mcp_server(request))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(|_| Json(()))
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}
