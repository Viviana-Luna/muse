//! 高频结构化运行日志的独立 SQLite 存储边界。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

const LOG_DATABASE_DIR: &str = "logs";
const LOG_DATABASE_FILE: &str = "runtime-usage.sqlite";
const LOG_DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum RuntimeLogStorageError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Integrity(String),
}

impl std::fmt::Display for RuntimeLogStorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "运行日志文件操作失败：{error}"),
            Self::Sqlite(error) => write!(formatter, "运行日志 SQLite 操作失败：{error}"),
            Self::Integrity(message) => write!(formatter, "运行日志完整性检查失败：{message}"),
        }
    }
}

impl std::error::Error for RuntimeLogStorageError {}

impl From<std::io::Error> for RuntimeLogStorageError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<rusqlite::Error> for RuntimeLogStorageError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

/// 返回独立运行日志数据库路径，不创建目录或文件。
pub fn runtime_log_database_path(base_dir: impl AsRef<Path>) -> PathBuf {
    base_dir
        .as_ref()
        .join(LOG_DATABASE_DIR)
        .join(LOG_DATABASE_FILE)
}

/// 创建或打开独立运行日志数据库。
pub fn open_runtime_log_database(
    base_dir: impl AsRef<Path>,
) -> Result<(PathBuf, Connection), RuntimeLogStorageError> {
    let path = runtime_log_database_path(base_dir);
    let connection = open_runtime_log_database_at_path(&path)?;
    Ok((path, connection))
}

/// 打开已知的独立运行日志数据库路径。
pub fn open_runtime_log_database_at_path(
    path: &Path,
) -> Result<Connection, RuntimeLogStorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("运行日志数据库路径缺少父目录"))?;
    ensure_private_directory(parent)?;
    reject_non_regular_file_path(path)?;
    let connection = Connection::open(path)?;
    restrict_private_file_permissions(path)?;
    connection.busy_timeout(LOG_DATABASE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

/// 验证日志库自身完整性；核心库备份和恢复不会调用该入口。
pub fn validate_runtime_log_database(
    connection: &Connection,
) -> Result<(), RuntimeLogStorageError> {
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(RuntimeLogStorageError::Integrity(format!(
            "SQLite quick_check 返回：{quick_check}"
        )));
    }
    let mut foreign_key_check = connection.prepare("PRAGMA foreign_key_check")?;
    if foreign_key_check.query([])?.next()?.is_some() {
        return Err(RuntimeLogStorageError::Integrity(
            "SQLite foreign_key_check 发现无效引用".to_string(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::other(format!(
                "日志路径 `{}` 不是普通目录",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(std::io::Error::other(format!(
                    "创建后的日志路径 `{}` 不是普通目录",
                    path.display()
                )));
            }
        }
        Err(error) => return Err(error),
    }
    restrict_private_directory_permissions(path)
}

fn reject_non_regular_file_path(path: &Path) -> Result<(), std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            std::io::Error::other(format!("日志文件 `{}` 不是普通文件", path.display())),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn restrict_private_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_private_directory_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn restrict_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_private_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}
