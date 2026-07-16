//! 系统凭据库适配层。
//!
//! 当前用于联网搜索以及旧模型、旧 MCP 凭据的一次性迁移。模型 Provider 与 MCP API Key
//! 的稳态事实源已经改为受保护的 `config.toml`，不再写入这里。

/// 系统凭据库操作失败。
#[derive(Debug)]
pub struct SecretStoreError(pub String);

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "系统凭据库操作失败：{}", self.0)
    }
}

impl std::error::Error for SecretStoreError {}

/// 基于当前操作系统凭据库的密钥存储。
///
/// macOS 使用 Keychain，Windows 使用 Credential Manager；具体后端由 `keyring`
/// 按平台选择。服务名保持稳定，方便升级时完成旧配置迁移。
#[derive(Clone)]
pub struct PlatformSecretStore {
    service: String,
}

/// 模型配置事务所需的最小凭据存储接口，便于使用内存后端验证崩溃阶段。
pub trait SecretStoreBackend {
    fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, account: &str) -> Result<(), SecretStoreError>;

    /// 写入后通过新的存储读取动作核对内容，确认持久化成功后才允许提交引用。
    fn set_verified(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
        self.set(account, value)?;
        match self.get_optional(account)? {
            Some(stored) if stored == value => Ok(()),
            Some(_) => Err(SecretStoreError(
                "凭据写入后的跨实例回读内容不一致".to_string(),
            )),
            None => Err(SecretStoreError(
                "凭据写入后的跨实例回读未找到新凭据".to_string(),
            )),
        }
    }
}

impl PlatformSecretStore {
    /// 创建指定产品服务名的凭据存储。
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// 读取密钥；凭据尚不存在时返回 `None`。
    pub fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretStoreError(err.to_string()))?;
        match entry.get_password() {
            Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
            Ok(_) => Ok(None),
            // `keyring` 会以 `NoEntry` 表示凭据尚未配置；这不是启动错误。
            Err(err) if is_missing_credential_error(&err) => Ok(None),
            Err(err) => Err(SecretStoreError(err.to_string())),
        }
    }

    /// 写入或更新密钥。
    pub fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretStoreError(err.to_string()))?;
        entry
            .set_password(value)
            .map_err(|err| SecretStoreError(err.to_string()))
    }

    /// 写入后用新建的 `Entry` 回读，避免把进程内 Mock 状态误判为持久化成功。
    pub fn set_verified(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
        SecretStoreBackend::set_verified(self, account, value)
    }

    /// 删除密钥；凭据本来就不存在时视为幂等成功。
    pub fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretStoreError(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(err) if is_missing_credential_error(&err) => Ok(()),
            Err(err) => Err(SecretStoreError(err.to_string())),
        }
    }
}

impl SecretStoreBackend for PlatformSecretStore {
    fn get_optional(&self, account: &str) -> Result<Option<String>, SecretStoreError> {
        PlatformSecretStore::get_optional(self, account)
    }

    fn set(&self, account: &str, value: &str) -> Result<(), SecretStoreError> {
        PlatformSecretStore::set(self, account, value)
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        PlatformSecretStore::delete(self, account)
    }
}

fn is_missing_credential_error(error: &keyring::Error) -> bool {
    matches!(error, keyring::Error::NoEntry)
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_os = "macos", feature = "live-tests"))]
    use super::PlatformSecretStore;
    use super::is_missing_credential_error;
    #[cfg(all(target_os = "macos", feature = "live-tests"))]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(all(target_os = "macos", feature = "live-tests"))]
    struct CredentialCleanup {
        store: PlatformSecretStore,
        account: String,
    }

    #[cfg(all(target_os = "macos", feature = "live-tests"))]
    impl Drop for CredentialCleanup {
        fn drop(&mut self) {
            let _ = self.store.delete(&self.account);
        }
    }

    #[test]
    fn missing_credential_is_recognized_as_optional() {
        assert!(is_missing_credential_error(&keyring::Error::NoEntry));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_entry_uses_native_keychain_credential() {
        let entry = keyring::Entry::new("Muse-native-backend-test", "backend")
            .expect("应能创建 macOS 凭据条目");
        assert!(
            entry.get_credential().is::<keyring::macos::MacCredential>(),
            "macOS 桌面构建不得退回 Mock Credential Store"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_entry_uses_native_credential_manager() {
        let entry = keyring::Entry::new("Muse-native-backend-test", "backend")
            .expect("应能创建 Windows 凭据条目");
        assert!(
            entry
                .get_credential()
                .is::<keyring::windows::WinCredential>(),
            "Windows 桌面构建不得退回 Mock Credential Store"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[cfg(feature = "live-tests")]
    fn macos_keychain_supports_cross_store_read_replace_and_delete() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间应可用")
            .as_nanos();
        let service = format!("Muse-native-credential-test-{suffix}");
        let account = format!("cross-entry-{suffix}");
        let first = PlatformSecretStore::new(&service);
        let _ = first.delete(&account);
        let _cleanup = CredentialCleanup {
            store: PlatformSecretStore::new(&service),
            account: account.clone(),
        };

        first
            .set_verified(&account, "first-test-secret")
            .expect("新凭据应能被另一 Entry 读回");
        drop(first);
        let second = PlatformSecretStore::new(&service);
        assert_eq!(
            second.get_optional(&account).expect("应读取首次保存值"),
            Some("first-test-secret".to_string())
        );

        second
            .set_verified(&account, "replacement-test-secret")
            .expect("替换后的凭据应能被另一 Entry 读回");
        drop(second);
        let third = PlatformSecretStore::new(&service);
        assert_eq!(
            third.get_optional(&account).expect("应读取替换值"),
            Some("replacement-test-secret".to_string())
        );
        third.delete(&account).expect("应删除隔离测试凭据");
        assert_eq!(
            PlatformSecretStore::new(&service)
                .get_optional(&account)
                .expect("删除后读取应成功"),
            None
        );
    }
}
