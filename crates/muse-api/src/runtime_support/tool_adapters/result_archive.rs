//! 大型工具结果的受控本地归档。
//!
//! 归档只接受服务端生成的安全标识，使用 `create_new` 防止覆盖，并在写入和读取时
//! 拒绝符号链接。HTTP/MCP 响应只能暴露逻辑资源标识，不能返回本机路径。

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

pub(in crate::runtime_support) fn is_safe_result_id(result_id: &str) -> bool {
    !result_id.trim().is_empty()
        && result_id.len() <= 160
        && result_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

pub(in crate::runtime_support) async fn write_new(
    result_id: &str,
    content: String,
) -> io::Result<()> {
    let root = archive_root();
    let result_id = result_id.to_string();
    tokio::task::spawn_blocking(move || write_new_at(&root, &result_id, content.as_bytes()))
        .await
        .map_err(|error| io::Error::other(format!("归档写入任务异常结束：{error}")))?
}

pub(in crate::runtime_support) async fn read(result_id: &str) -> io::Result<String> {
    let root = archive_root();
    let result_id = result_id.to_string();
    tokio::task::spawn_blocking(move || read_at(&root, &result_id))
        .await
        .map_err(|error| io::Error::other(format!("归档读取任务异常结束：{error}")))?
}

fn archive_root() -> PathBuf {
    muse_core::config::Config::config_dir()
        .join("harness")
        .join("tool-results")
}

fn archive_path(root: &Path, result_id: &str) -> io::Result<PathBuf> {
    if !is_safe_result_id(result_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "工具结果标识含非法字符。",
        ));
    }
    Ok(root.join(format!("{result_id}.json")))
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata_is_reparse(&metadata) || !metadata.is_dir() {
            return Err(io::Error::other("工具结果归档目录不是可信普通目录。"));
        }
    } else {
        fs::create_dir(path)?;
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn ensure_archive_root(root: &Path) -> io::Result<()> {
    let Some(parent) = root.parent() else {
        return Err(io::Error::other("工具结果归档目录缺少父目录。"));
    };
    let Some(config_root) = parent.parent() else {
        return Err(io::Error::other("工具结果归档目录缺少数据根目录。"));
    };
    if !config_root.exists() {
        fs::create_dir_all(config_root)?;
    }
    let config_metadata = fs::symlink_metadata(config_root)?;
    if metadata_is_reparse(&config_metadata) || !config_metadata.is_dir() {
        return Err(io::Error::other("应用数据根目录不是可信普通目录。"));
    }
    ensure_private_directory(parent)?;
    ensure_private_directory(root)
}

fn write_new_at(root: &Path, result_id: &str, content: &[u8]) -> io::Result<()> {
    let content_len = u64::try_from(content.len()).unwrap_or(u64::MAX);
    if content_len > MAX_ARCHIVE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "工具结果归档超过 64 MiB 安全上限。",
        ));
    }
    ensure_archive_root(root)?;
    let path = archive_path(root, result_id)?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(WINDOWS_OPEN_REPARSE_POINT);
    let mut file = options.open(&path)?;
    file.write_all(content)?;
    file.sync_all()?;
    sync_directory(root)
}

fn read_at(root: &Path, result_id: &str) -> io::Result<String> {
    let path = archive_path(root, result_id)?;
    let link_metadata = fs::symlink_metadata(&path)?;
    if metadata_is_reparse(&link_metadata) || !link_metadata.is_file() {
        return Err(io::Error::other("工具结果归档不是可信普通文件。"));
    }
    if link_metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "工具结果归档超过 64 MiB 读取上限。",
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(WINDOWS_OPEN_REPARSE_POINT);
    let mut file = options.open(&path)?;
    let metadata = file.metadata()?;
    if metadata_is_reparse(&metadata) || !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "工具结果归档文件类型或大小无效。",
        ));
    }
    let capacity = usize::try_from(metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(MAX_ARCHIVE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ARCHIVE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "工具结果归档读取过程中超过安全上限。",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "工具结果归档不是 UTF-8。"))
}

#[cfg(windows)]
const WINDOWS_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "muse-tool-result-archive-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn result_id_rejects_traversal_and_unbounded_input() {
        assert!(is_safe_result_id("tool-result-123_abc"));
        assert!(!is_safe_result_id("../secret"));
        assert!(!is_safe_result_id("result.json"));
        assert!(!is_safe_result_id(&"a".repeat(161)));
    }

    #[test]
    fn archive_is_create_only_and_round_trips() {
        let base = temp_root("roundtrip");
        let root = base.join("harness").join("tool-results");
        fs::create_dir_all(&base).unwrap();

        write_new_at(&root, "result-1", br#"{"value":"ok"}"#).unwrap();
        assert_eq!(read_at(&root, "result-1").unwrap(), r#"{"value":"ok"}"#);
        let duplicate = write_new_at(&root, "result-1", b"replacement").unwrap_err();
        assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn archive_rejects_symlink_directory_and_file() {
        use std::os::unix::fs::symlink;

        let base = temp_root("symlink");
        let target = temp_root("target");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&target).unwrap();
        symlink(&target, base.join("harness")).unwrap();
        let root = base.join("harness").join("tool-results");
        assert!(write_new_at(&root, "result-1", b"secret").is_err());

        fs::remove_file(base.join("harness")).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(target.join("outside.json"), "outside").unwrap();
        symlink(target.join("outside.json"), root.join("result-2.json")).unwrap();
        assert!(read_at(&root, "result-2").is_err());

        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(target).unwrap();
    }
}
