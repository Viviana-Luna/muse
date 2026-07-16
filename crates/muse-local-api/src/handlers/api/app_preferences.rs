/// 读取用户级外观偏好与配置诊断。
pub(crate) async fn handle_get_appearance_preferences(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AppearancePreferencesResponse>, (StatusCode, Json<ConfigMutationErrorResponse>)> {
    let (changed, models, snapshot) = {
        let mut store = state.user_config.lock().await;
        let changed = store
            .refresh_from_disk()
            .map_err(app_preferences_error_response)?;
        (changed, store.config().clone(), store.snapshot())
    };
    if changed {
        rebuild_models_after_preferences_refresh(&state, &models).await?;
    }
    Ok(Json(AppearancePreferencesResponse::from(snapshot)))
}

/// 原子更新用户级外观偏好；其他 TOML 字段、注释和顺序保持不变。
pub(crate) async fn handle_put_appearance_preferences(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AppearancePreferencesUpdateRequest>,
) -> Result<Json<AppearancePreferencesResponse>, (StatusCode, Json<ConfigMutationErrorResponse>)> {
    let (result, models) = {
        let mut store = state.user_config.lock().await;
        let mut appearance = store.snapshot().config.appearance;
        appearance.background_blur = request.background_blur;
        appearance.background_opacity = request.background_opacity;
        appearance.motion_level = request.motion_level;
        let result = store.update_appearance(appearance);
        (result, store.config().clone())
    };
    if matches!(
        &result,
        Err(muse_core::app::preferences::MuseConfigStoreError::Conflict(_))
    ) {
        rebuild_models_after_preferences_refresh(&state, &models).await?;
    }
    let snapshot = result.map_err(app_preferences_error_response)?;
    Ok(Json(AppearancePreferencesResponse::from(snapshot)))
}

/// 外观与模型共享同一 TOML revision；任一入口观察到手工改动后都必须同步运行时。
async fn rebuild_models_after_preferences_refresh(
    state: &Arc<AppState>,
    models: &ModelsConfig,
) -> Result<(), (StatusCode, Json<ConfigMutationErrorResponse>)> {
    let provider = build_chat_provider(&models.chat).map_err(|_| {
        app_preferences_runtime_error("手工模型配置无法应用，请检查 Provider Profile。")
    })?;
    *state.provider.lock().await = provider;
    rebuild_tts_provider_from_state(state).await;
    rebuild_speech_provider_from_state(state).await;
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct AppearancePreferencesUpdateRequest {
    background_blur: u8,
    background_opacity: f64,
    motion_level: muse_core::app::preferences::MotionLevel,
}

#[derive(Serialize)]
pub(crate) struct AppearancePreferencesResponse {
    schema_version: u32,
    appearance: muse_core::app::preferences::AppearancePreferences,
    diagnostics: Vec<muse_core::app::preferences::ConfigDiagnostic>,
}

#[derive(Serialize)]
pub(crate) struct ConfigMutationErrorResponse {
    error: String,
    code: String,
    field_path: String,
    message: String,
}

impl From<muse_core::app::preferences::MuseConfigSnapshot>
    for AppearancePreferencesResponse
{
    fn from(snapshot: muse_core::app::preferences::MuseConfigSnapshot) -> Self {
        Self {
            schema_version: snapshot.config.schema_version,
            appearance: snapshot.config.appearance,
            diagnostics: snapshot.diagnostics,
        }
    }
}

fn app_preferences_error_response(
    error: muse_core::app::preferences::MuseConfigStoreError,
) -> (StatusCode, Json<ConfigMutationErrorResponse>) {
    use muse_core::app::preferences::{ConfigDiagnostic, MuseConfigStoreError};

    let (status, diagnostic) = match error {
        MuseConfigStoreError::Validation(diagnostic) => (StatusCode::BAD_REQUEST, diagnostic),
        MuseConfigStoreError::Conflict(diagnostic) => (StatusCode::CONFLICT, diagnostic),
        MuseConfigStoreError::Io(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ConfigDiagnostic {
                code: "config_io_failed".to_string(),
                field_path: "config.toml".to_string(),
                message: "用户配置文件读写失败，请检查文件权限和磁盘状态。".to_string(),
            },
        ),
        MuseConfigStoreError::Parse(_) | MuseConfigStoreError::Serialize(_) => (
            StatusCode::BAD_REQUEST,
            ConfigDiagnostic {
                code: "config_parse_failed".to_string(),
                field_path: "config.toml".to_string(),
                message: "config.toml 语法无效，请修正手工编辑内容后重试。".to_string(),
            },
        ),
        MuseConfigStoreError::UnsupportedVersion(_) => (
            StatusCode::CONFLICT,
            ConfigDiagnostic {
                code: "config_version_unsupported".to_string(),
                field_path: "schema_version".to_string(),
                message: "config.toml 版本高于当前 Muse 支持范围，已拒绝覆盖。".to_string(),
            },
        ),
    };
    let message = diagnostic.message;
    (
        status,
        Json(ConfigMutationErrorResponse {
            error: message.clone(),
            code: diagnostic.code,
            field_path: diagnostic.field_path,
            message,
        }),
    )
}

fn app_preferences_runtime_error(
    message: &str,
) -> (StatusCode, Json<ConfigMutationErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ConfigMutationErrorResponse {
            error: message.to_string(),
            code: "config_model_profile_invalid".to_string(),
            field_path: "providers".to_string(),
            message: message.to_string(),
        }),
    )
}
