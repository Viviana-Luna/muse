//! 语音合成、语音识别与能力查询接口。

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use super::ApiRouter;
use crate::handlers;

const SPEECH_UPLOAD_BODY_LIMIT_BYTES: usize = 25 * 1024 * 1024 + 64 * 1024;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route("/tts", post(handlers::handle_tts_runtime_route))
        .route(
            "/speech/transcribe",
            post(handlers::handle_speech_transcribe)
                .layer(DefaultBodyLimit::max(SPEECH_UPLOAD_BODY_LIMIT_BYTES)),
        )
        .route(
            "/voice/capabilities",
            get(handlers::handle_voice_capabilities),
        )
}
