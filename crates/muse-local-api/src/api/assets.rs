//! Persona 资源上传与读取接口，包含路由注册和 HTTP 适配实现。

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::fs;

use super::ApiRouter;
use crate::asset_support::{
    content_hash_hex, discard_unreferenced_uploaded_asset, persist_uploaded_asset,
    uploaded_assets_dir, uploaded_image_extension, uploaded_image_extension_from_bytes,
    validated_uploaded_asset_name,
};
use crate::dto::{AssetUploadResponse, ErrorResponse};
use crate::error::{bad_request, internal_error};
use crate::state::AppState;

const PERSONA_ASSET_UPLOAD_BODY_LIMIT_BYTES: usize = 6 * 1024 * 1024;
const MAX_PERSONA_IMAGE_BYTES: usize = 5 * 1024 * 1024;

pub(super) fn routes() -> ApiRouter {
    Router::new()
        .route(
            "/assets/upload",
            post(handle_upload_asset)
                .layer(DefaultBodyLimit::max(PERSONA_ASSET_UPLOAD_BODY_LIMIT_BYTES)),
        )
        .route(
            "/assets/uploaded/{filename}",
            get(handle_uploaded_asset).delete(handle_discard_uploaded_asset),
        )
}

/// 上传角色图片资源，并返回可由前端直接引用的本地资源 URL。
async fn handle_upload_asset(
    mut multipart: Multipart,
) -> Result<Json<AssetUploadResponse>, (StatusCode, Json<ErrorResponse>)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| bad_request(&format!("读取上传表单失败：{err}")))?
    {
        if field.name() != Some("file") {
            continue;
        }

        let content_type = field.content_type().map(str::to_string);
        let declared_extension = uploaded_image_extension(content_type.as_deref());
        let bytes = field
            .bytes()
            .await
            .map_err(|err| bad_request(&format!("读取上传文件失败：{err}")))?;
        if bytes.is_empty() {
            return Err(bad_request("上传图片不能为空。"));
        }
        if bytes.len() > MAX_PERSONA_IMAGE_BYTES {
            return Err(bad_request("上传图片不能超过 5MB。"));
        }
        let extension = uploaded_image_extension_from_bytes(&bytes)
            .ok_or_else(|| bad_request("角色图片内容不是有效的 PNG、JPEG 或 WebP 格式。"))?;
        if declared_extension.is_some_and(|declared| declared != extension) {
            return Err(bad_request("上传图片声明的类型与文件内容不一致。"));
        }

        let filename = format!("{}.{}", content_hash_hex(&bytes), extension);
        let dir = uploaded_assets_dir();
        fs::create_dir_all(&dir)
            .await
            .map_err(|err| internal_error(format!("创建角色图片目录失败：{err}")))?;
        let path = dir.join(&filename);
        persist_uploaded_asset(&path, &bytes)
            .await
            .map_err(internal_error)?;

        return Ok(Json(AssetUploadResponse {
            url: format!("/api/assets/uploaded/{filename}"),
        }));
    }

    Err(bad_request("请在 `file` 字段中上传角色图片。"))
}

/// 经鉴权读取单个上传图片；不暴露目录浏览，也不跟随符号链接。
async fn handle_uploaded_asset(
    Path(filename): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let (expected_hash, extension) = validated_uploaded_asset_name(&filename)
        .ok_or_else(|| bad_request("上传资源文件名无效。"))?;
    let path = uploaded_assets_dir().join(&filename);
    let bytes = match tokio::task::spawn_blocking(move || read_uploaded_asset_file(&path)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "上传资源不存在。".to_string(),
                }),
            ));
        }
        Ok(Err(error)) => return Err(internal_error(format!("读取上传资源失败：{error}"))),
        Err(error) => return Err(internal_error(format!("上传资源读取任务异常结束：{error}"))),
    };
    let detected_extension = uploaded_image_extension_from_bytes(&bytes);
    if detected_extension != Some(extension) || content_hash_hex(&bytes) != expected_hash {
        return Err(internal_error("上传资源完整性校验失败。".to_string()));
    }
    let content_type = match extension {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => return Err(internal_error("上传资源类型无效。".to_string())),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header(header::ETAG, format!("\"{expected_hash}\""))
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|error| internal_error(format!("构造上传资源响应失败：{error}")))
}

/// 回滚尚未被任何角色展示包引用的上传图片。
///
/// 内容哈希文件可能已经被其他角色复用，因此“仍被引用”和“文件已不存在”都视为
/// 回滚目标已经满足；只有确认无引用时才删除磁盘文件。
async fn handle_discard_uploaded_asset(
    Path(filename): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    validated_uploaded_asset_name(&filename).ok_or_else(|| bad_request("上传资源文件名无效。"))?;
    let url = format!("/api/assets/uploaded/{filename}");
    discard_unreferenced_uploaded_asset(&state, &url)
        .await
        .map_err(|error| internal_error(format!("回滚无引用上传资源失败：{error}")))?;
    Ok(StatusCode::NO_CONTENT)
}

fn read_uploaded_asset_file(path: &StdPath) -> std::io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "上传资源不是普通文件。",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "上传资源不能是 Windows reparse point。",
            ));
        }
    }
    if metadata.len() == 0 || metadata.len() > MAX_PERSONA_IMAGE_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "上传资源大小超出安全范围。",
        ));
    }
    let mut reader = std::io::Read::take(file, MAX_PERSONA_IMAGE_BYTES as u64 + 1);
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::read_to_end(&mut reader, &mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_PERSONA_IMAGE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "上传资源读取过程中超出安全范围。",
        ));
    }
    Ok(bytes)
}
