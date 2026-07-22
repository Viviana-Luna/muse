//! 将领域层和运行时错误转换为稳定的 HTTP 响应。

use axum::{Json, http::StatusCode};
use muse_core::domain::persona::character::card::PersonaCardError;
use muse_core::domain::persona::character::store::PersonaStoreError;
use muse_core::domain::persona::visual::store::VisualPackStoreError;

use crate::dto::ErrorResponse;

pub fn voice_error_response(
    err: muse_core::speech::VoiceError,
) -> (StatusCode, Json<ErrorResponse>) {
    let status = match &err {
        muse_core::speech::VoiceError::ConfigError(_) => StatusCode::SERVICE_UNAVAILABLE,
        muse_core::speech::VoiceError::NoSpeech(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    tracing::warn!(
        target: "agent_vp::voice",
        status = %status,
        error = %err,
        "语音链路调用失败"
    );
    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

pub fn model_catalog_error_response(
    err: muse_core::model::catalog::ModelCatalogError,
) -> (StatusCode, Json<ErrorResponse>) {
    let status = match &err {
        muse_core::model::catalog::ModelCatalogError::Validation(_) => StatusCode::BAD_REQUEST,
        muse_core::model::catalog::ModelCatalogError::NotFound(_) => StatusCode::NOT_FOUND,
        muse_core::model::catalog::ModelCatalogError::Conflict(_) => StatusCode::CONFLICT,
        muse_core::model::catalog::ModelCatalogError::Config(
            muse_core::app::preferences::MuseConfigStoreError::Conflict(_),
        ) => StatusCode::CONFLICT,
        muse_core::model::catalog::ModelCatalogError::Config(
            muse_core::app::preferences::MuseConfigStoreError::Validation(_),
        ) => StatusCode::BAD_REQUEST,
        muse_core::model::catalog::ModelCatalogError::Io(_)
        | muse_core::model::catalog::ModelCatalogError::Sqlite(_)
        | muse_core::model::catalog::ModelCatalogError::Storage(_)
        | muse_core::model::catalog::ModelCatalogError::Config(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let error = match err {
        muse_core::model::catalog::ModelCatalogError::Config(
            muse_core::app::preferences::MuseConfigStoreError::Conflict(diagnostic),
        )
        | muse_core::model::catalog::ModelCatalogError::Config(
            muse_core::app::preferences::MuseConfigStoreError::Validation(diagnostic),
        ) => format!("{}：{}", diagnostic.code, diagnostic.message),
        muse_core::model::catalog::ModelCatalogError::Config(_) => {
            "config_write_failed：保存 config.toml 失败，请检查语法、权限和磁盘状态。".to_string()
        }
        other => other.to_string(),
    };
    (status, Json(ErrorResponse { error }))
}

pub fn bad_request(message: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
}

pub fn internal_error(message: String) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse { error: message }),
    )
}

pub fn persona_store_error_response(err: PersonaStoreError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match err {
        PersonaStoreError::PersonaNotFound(_) => StatusCode::NOT_FOUND,
        PersonaStoreError::DuplicateId(_) | PersonaStoreError::Validation(_) => {
            StatusCode::BAD_REQUEST
        }
        PersonaStoreError::Io(_)
        | PersonaStoreError::Serde(_)
        | PersonaStoreError::ActivePersonaMissing(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };

    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

pub fn visual_pack_store_error_response(
    err: VisualPackStoreError,
) -> (StatusCode, Json<ErrorResponse>) {
    let status = match err {
        VisualPackStoreError::DuplicateId(_) | VisualPackStoreError::Validation(_) => {
            StatusCode::BAD_REQUEST
        }
        VisualPackStoreError::Io(_) | VisualPackStoreError::Serde(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };

    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

pub fn persona_card_error_response(err: PersonaCardError) -> (StatusCode, Json<ErrorResponse>) {
    let status = match err {
        PersonaCardError::Conflict(_) => StatusCode::CONFLICT,
        PersonaCardError::UnsupportedSchemaVersion(_)
        | PersonaCardError::Validation(_)
        | PersonaCardError::VisualPackValidation(_) => StatusCode::BAD_REQUEST,
    };

    (
        status,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}
