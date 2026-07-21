use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;
use tauri::AppHandle;
use tower_http::cors::{Any, CorsLayer};
use crate::AppState;
use super::handlers::*;

pub fn create_router(app: AppHandle, state: Arc<AppState>) -> Router {
    let shared = SharedState { app: app.clone(), state: state.clone() };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        // OpenAI Chat Completions
        .route("/v1/chat/completions", post(handle_chat_completions))
        // OpenAI Completions (legacy)
        .route("/v1/completions", post(handle_completions))
        // OpenAI Responses API
        .route("/v1/responses", post(handle_responses))
        // OpenAI Embeddings
        .route("/v1/embeddings", post(handle_embeddings))
        // OpenAI Models
        .route("/v1/models", get(handle_list_models))
        // OpenAI Images
        .route("/v1/images/generations", post(handle_images))
        // OpenAI Audio
        .route("/v1/audio/transcriptions", post(handle_audio_transcriptions))
        .route("/v1/audio/speech", post(handle_audio_speech))
        // Anthropic Messages API
        .route("/v1/messages", post(handle_messages))
        // Health check
        .route("/health", get(handle_health))
        .layer(cors)
        .with_state(shared)
}

#[derive(Clone)]
pub struct SharedState {
    pub app: AppHandle,
    pub state: Arc<AppState>,
}
