//! 上传资源的持久化、校验与引用清理。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use muse_core::domain::persona::visual::VisualPack;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::state::AppState;

pub(crate) fn uploaded_assets_dir() -> PathBuf {
    muse_core::config::Config::config_dir()
        .join("assets")
        .join("uploaded")
}

pub(crate) async fn persist_uploaded_asset(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
    {
        Ok(mut file) => {
            let result = async {
                file.write_all(bytes)
                    .await
                    .map_err(|error| format!("保存角色图片失败：{error}"))?;
                file.sync_data()
                    .await
                    .map_err(|error| format!("同步角色图片失败：{error}"))
            }
            .await;
            drop(file);
            if result.is_err() {
                let _ = fs::remove_file(path).await;
            }
            result
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .await
                .map_err(|error| format!("检查已有角色图片失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("角色图片目标已被非普通文件占用。".to_string());
            }
            let existing = fs::read(path)
                .await
                .map_err(|error| format!("读取已有角色图片失败：{error}"))?;
            if existing != bytes {
                return Err("已有角色图片与内容哈希不一致。".to_string());
            }
            Ok(())
        }
        Err(error) => Err(format!("创建角色图片失败：{error}")),
    }
}

pub(crate) fn validated_uploaded_asset_name(filename: &str) -> Option<(&str, &str)> {
    let (digest, extension) = filename.rsplit_once('.')?;
    let valid_digest = digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    (valid_digest && matches!(extension, "png" | "jpg" | "webp")).then_some((digest, extension))
}

pub(crate) fn uploaded_image_extension(content_type: Option<&str>) -> Option<&'static str> {
    match content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

pub(crate) fn uploaded_image_extension_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

pub(crate) fn content_hash_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn visual_pack_paths(visual_pack: &VisualPack) -> Vec<String> {
    [
        &visual_pack.portrait_path,
        &visual_pack.background_path,
        &visual_pack.avatar_path,
    ]
    .into_iter()
    .filter(|path| !path.trim().is_empty())
    .cloned()
    .collect()
}

pub(crate) enum UploadedAssetDiscardOutcome {
    Deleted,
    RetainedReference,
    Missing,
}

pub(crate) async fn discard_unreferenced_uploaded_asset(
    state: &Arc<AppState>,
    url: &str,
) -> Result<UploadedAssetDiscardOutcome, String> {
    let filename = url
        .strip_prefix("/api/assets/uploaded/")
        .filter(|filename| validated_uploaded_asset_name(filename).is_some())
        .ok_or_else(|| "上传资源地址格式无效。".to_string())?;
    let visual_packs = state.visual_packs.lock().await;
    if visual_packs
        .visual_packs()
        .iter()
        .flat_map(visual_pack_paths)
        .any(|path| path == url)
    {
        return Ok(UploadedAssetDiscardOutcome::RetainedReference);
    }

    let path = uploaded_assets_dir().join(filename);
    match fs::remove_file(path).await {
        Ok(()) => Ok(UploadedAssetDiscardOutcome::Deleted),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(UploadedAssetDiscardOutcome::Missing)
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) async fn cleanup_unreferenced_uploaded_assets(
    state: &Arc<AppState>,
    candidates: Vec<String>,
) {
    if candidates.is_empty() {
        return;
    }
    for url in candidates.into_iter().collect::<HashSet<_>>() {
        match discard_unreferenced_uploaded_asset(state, &url).await {
            Ok(UploadedAssetDiscardOutcome::Deleted) => tracing::info!(
                target: "muse::persona_assets",
                asset_url = %url,
                "已清理无角色展示包引用的上传资源"
            ),
            Ok(
                UploadedAssetDiscardOutcome::RetainedReference
                | UploadedAssetDiscardOutcome::Missing,
            ) => {}
            Err(error) => tracing::warn!(
                target: "muse::persona_assets",
                asset_url = %url,
                error = %error,
                "角色已保存，但无引用上传资源清理失败；保留文件等待后续诊断"
            ),
        }
    }
}
