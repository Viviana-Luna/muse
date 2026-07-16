//! Muse 原生桌面入口。
//!
//! UI 由 Tauri 内置资源协议加载，API 则监听本进程随机创建的回环端口。端口和访问令牌
//! 只通过受控命令交付给主窗口，不进入 URL、命令行参数或持久化配置。

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use muse_core::config::{Config, DataDirLock, LegacyWorkspaceMigrationReport};
use muse_local_api::{LocalApiBootstrap, LocalApiSecurity, LocalApiSecurityOptions};
use tauri::http::HeaderValue;
use tauri::utils::config::{Csp, CspDirectiveSources};
use tauri::{Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::oneshot;

const MAIN_WINDOW_LABEL: &str = "muse";

const SINGLE_INSTANCE_CI_MARKER_ENV: &str = "MUSE_DESKTOP_CI_SINGLE_INSTANCE_MARKER_FILE";
const SINGLE_INSTANCE_CI_MARKER_SCHEMA: &str = "muse-single-instance-probe/v1";

struct DesktopRuntimeState {
    bootstrap: LocalApiBootstrap,
    data_dir: PathBuf,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    _instance_lock: DataDirLock,
}

struct StartedLocalServer {
    bootstrap: LocalApiBootstrap,
    shutdown: oneshot::Sender<()>,
}

struct DesktopDataPaths {
    data_dir: PathBuf,
    legacy_dir: Option<PathBuf>,
}

struct PreparedDesktopData {
    instance_lock: DataDirLock,
    migration: Option<LegacyWorkspaceMigrationReport>,
}

/// 仅允许主窗口读取本次进程的 API 启动信息。
#[tauri::command]
fn runtime_bootstrap(
    webview_window: WebviewWindow,
    state: State<'_, DesktopRuntimeState>,
) -> Result<LocalApiBootstrap, String> {
    if webview_window.label() != MAIN_WINDOW_LABEL {
        return Err("当前窗口无权读取本地 API 启动信息。".to_string());
    }
    Ok(state.bootstrap.clone())
}

/// 由内嵌 UI 在完成 Bootstrap 和鉴权 health 后提交无密钥就绪证明。
///
/// 正常用户环境不会设置 `MUSE_DESKTOP_READY_FILE`，此命令仅完成一致性校验；打包
/// CI 设置该路径后可据此区分“进程存活”和“内嵌首页/API 主链路真实可用”。
#[tauri::command]
fn runtime_ready(
    webview_window: WebviewWindow,
    state: State<'_, DesktopRuntimeState>,
    protocol_version: String,
    instance_id: String,
) -> Result<(), String> {
    if webview_window.label() != MAIN_WINDOW_LABEL {
        return Err("当前窗口无权提交桌面就绪状态。".to_string());
    }
    if protocol_version != state.bootstrap.protocol_version
        || instance_id != state.bootstrap.instance_id
    {
        return Err("桌面就绪状态与当前本地 API 实例不一致。".to_string());
    }
    let Some(path) = std::env::var_os("MUSE_DESKTOP_READY_FILE").map(PathBuf::from) else {
        return Ok(());
    };
    write_desktop_ready_file(&path, &protocol_version, &instance_id, &state.data_dir)
}

fn write_desktop_ready_file(
    path: &std::path::Path,
    protocol_version: &str,
    instance_id: &str,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "protocol_version": protocol_version,
        "instance_id": instance_id,
        "data_dir": data_dir,
    }))
    .map_err(|error| format!("序列化桌面就绪状态失败：{error}"))?;
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("桌面就绪文件已被非普通文件占用。".to_string());
        }
        let existing =
            std::fs::read(path).map_err(|error| format!("读取已有桌面就绪文件失败：{error}"))?;
        return (existing == payload)
            .then_some(())
            .ok_or_else(|| "已有桌面就绪文件属于其他运行实例。".to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建桌面就绪文件目录失败：{error}"))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("创建桌面就绪文件失败：{error}"))?;
    file.write_all(&payload)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("同步桌面就绪文件失败：{error}"))
}

/// 仅在打包 CI 显式设置 marker 路径时，记录由主实例收到的第二次启动通知。
///
/// marker 不包含环境变量、Bearer 或运行时 Bootstrap；命令行中疑似凭据的参数会整体
/// 脱敏。文件必须位于已存在的绝对目录中，并以 create_new + 0600（Unix）方式创建，
/// 从而让冒烟测试能够区分“官方单实例回调成功”和“第二实例因数据锁失败退出”。
fn write_single_instance_ci_marker(
    path: &std::path::Path,
    args: &[String],
    cwd: &str,
) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("单实例 CI marker 必须使用绝对路径。".to_string());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "单实例 CI marker 缺少父目录。".to_string())?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|error| format!("读取单实例 CI marker 目录失败：{error}"))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err("单实例 CI marker 父路径必须是已存在的普通目录。".to_string());
    }

    let redacted_args = redact_single_instance_args(args);
    let payload = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": SINGLE_INSTANCE_CI_MARKER_SCHEMA,
        "cwd": cwd,
        "args": redacted_args,
    }))
    .map_err(|error| format!("序列化单实例 CI marker 失败：{error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("创建单实例 CI marker 失败：{error}"))?;
    file.write_all(&payload)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("同步单实例 CI marker 失败：{error}"))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("同步单实例 CI marker 目录失败：{error}"))?;
    Ok(())
}

fn redact_single_instance_args(args: &[String]) -> Vec<String> {
    let mut redact_next = false;
    args.iter()
        .map(|argument| {
            if redact_next {
                redact_next = false;
                return "[已脱敏参数]".to_string();
            }
            let key = argument
                .split_once('=')
                .map_or(argument.as_str(), |(key, _)| key);
            let normalized = key
                .trim_start_matches(&['-', '/'][..])
                .replace('-', "_")
                .to_ascii_uppercase();
            let sensitive = [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "PASSWD",
                "AUTHORIZATION",
                "BEARER",
                "API_KEY",
            ]
            .iter()
            .any(|marker| normalized.contains(marker));
            if sensitive {
                redact_next = !argument.contains('=');
                "[已脱敏参数]".to_string()
            } else {
                argument.clone()
            }
        })
        .collect()
}

/// 启动随机回环端口上的 API 服务。
async fn start_local_server(dev_url: Option<tauri::Url>) -> Result<StartedLocalServer, String> {
    let data_dir = Config::data_dir();
    let (session_store, migration) = muse_runtime::session::SessionStore::open(&data_dir)
        .await
        .map_err(|err| format!("初始化或迁移会话存储失败：{err}"))?;
    if migration.legacy_sources > 0 {
        tracing::info!(
            target: "muse::desktop",
            migrated_records = migration.migrated_records,
            quarantined_records = migration.quarantined_records,
            "legacy transcript 已迁移并保留备份"
        );
    }
    let mut config = Config::default();
    match muse_local_api::migrate_pristine_legacy_default_persona(
        &data_dir,
        &session_store,
        migration.quarantined_records,
    )
    .await
    {
        Ok(muse_local_api::LegacyDefaultPersonaMigrationOutcome::Removed { backup_path }) => {
            tracing::info!(
                target: "muse::desktop",
                backup_path = %backup_path.display(),
                "已备份并移除未使用的历史自动默认角色"
            );
        }
        Ok(muse_local_api::LegacyDefaultPersonaMigrationOutcome::PreservedUncertain { reason }) => {
            tracing::warn!(
                target: "muse::desktop",
                %reason,
                "无法证明历史自动默认角色未被使用，已原样保留"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                target: "muse::desktop",
                %error,
                "历史自动默认角色迁移失败，已原样保留"
            );
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| format!("无法绑定本地桌面服务端口：{err}"))?;
    let address = listener
        .local_addr()
        .map_err(|err| format!("无法读取本地桌面服务地址：{err}"))?;
    let expected_host = address.to_string();
    #[cfg(target_os = "macos")]
    let mut allowed_origins = vec!["tauri://localhost".to_string()];
    #[cfg(not(target_os = "macos"))]
    let mut allowed_origins = vec!["http://tauri.localhost".to_string()];
    if cfg!(dev)
        && let Some(dev_url) = dev_url.as_ref()
    {
        allowed_origins.push(dev_url.origin().ascii_serialization());
    }
    let security = LocalApiSecurity::new(LocalApiSecurityOptions {
        expected_host,
        allowed_origins,
        allow_loopback_dev_origins: cfg!(dev),
        access_token: None,
    })
    .map_err(|err| format!("无法创建本地 API 安全上下文：{err}"))?;
    let bootstrap = security.bootstrap();

    config.server.host = "127.0.0.1".to_string();
    config.server.port = address.port();
    let router = muse_local_api::build_router_with_security(config, security)
        .await
        .map_err(|err| format!("无法初始化 Muse 运行时：{err}"))?;
    let (shutdown, shutdown_rx) = oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(err) = server.await {
            tracing::error!(target: "muse::desktop", error = %err, "本地桌面 API 已停止");
        }
    });

    Ok(StartedLocalServer {
        bootstrap,
        shutdown,
    })
}

fn inject_runtime_csp(
    response: &mut tauri::http::Response<std::borrow::Cow<'static, [u8]>>,
    api_origin: &str,
) {
    let Some(csp_header) = response.headers_mut().get_mut("Content-Security-Policy") else {
        return;
    };
    let Ok(current_policy) = csp_header.to_str() else {
        tracing::warn!(target: "muse::desktop", "忽略无法解析的内置页面 CSP");
        return;
    };

    let mut directives: HashMap<String, CspDirectiveSources> =
        Csp::Policy(current_policy.to_string()).into();
    let connect_sources = directives.entry("connect-src".to_string()).or_default();
    if !connect_sources.contains(api_origin) {
        connect_sources.push(api_origin);
    }
    let ws_origin = api_origin.replacen("http://", "ws://", 1);
    if !connect_sources.contains(&ws_origin) {
        connect_sources.push(&ws_origin);
    }

    match HeaderValue::from_str(&Csp::from(directives).to_string()) {
        Ok(value) => *csp_header = value,
        Err(err) => tracing::error!(
            target: "muse::desktop",
            error = %err,
            "无法注入本地 API CSP"
        ),
    }
}

fn is_trusted_app_navigation(url: &tauri::Url, dev_url: Option<&tauri::Url>) -> bool {
    if cfg!(dev)
        && dev_url.is_some_and(|dev_url| {
            dev_url.scheme() == "http"
                && dev_url.host_str() == Some("127.0.0.1")
                && dev_url.port().is_some()
                && url.origin() == dev_url.origin()
        })
    {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        url.scheme() == "tauri" && url.host_str() == Some("localhost")
    }
    #[cfg(not(target_os = "macos"))]
    {
        url.scheme() == "http" && url.host_str() == Some("tauri.localhost")
    }
}

fn resolve_runtime_data_paths() -> Result<DesktopDataPaths, Box<dyn std::error::Error>> {
    let paths = if let Some(configured) = std::env::var_os("MUSE_DATA_DIR") {
        if configured.is_empty() {
            return Err("MUSE_DATA_DIR 不能为空。".into());
        }
        DesktopDataPaths {
            data_dir: PathBuf::from(configured),
            legacy_dir: None,
        }
    } else {
        let data_dir = Config::default_user_data_dir()?;
        let legacy_dir = std::env::current_dir()
            .ok()
            .and_then(|current_dir| recognized_legacy_workspace_dir(&current_dir));
        DesktopDataPaths {
            data_dir,
            legacy_dir,
        }
    };

    Ok(paths)
}

/// legacy 自动迁移只识别真实仓库根目录，不能采用任意 CWD 或 crate 子目录的数据。
fn recognized_legacy_workspace_dir(current_dir: &std::path::Path) -> Option<PathBuf> {
    Config::discover_legacy_workspace_data(current_dir)
}

fn prepare_runtime_data(
    paths: &DesktopDataPaths,
) -> Result<PreparedDesktopData, Box<dyn std::error::Error>> {
    // 目标锁必须先于任何检查或复制取得，且由 DesktopRuntimeState 持有到进程退出。
    let instance_lock = Config::acquire_data_dir_lock(&paths.data_dir)?;
    let migration = match paths.legacy_dir.as_deref() {
        Some(legacy_dir) => {
            // legacy 源锁覆盖完整的校验、staging 复制和原子提交。
            let legacy_lock = Config::acquire_data_dir_lock(legacy_dir)?;
            let migration = Config::migrate_legacy_workspace_data(&paths.data_dir, legacy_dir)?;
            drop(legacy_lock);
            migration
        }
        None => {
            // 显式 MUSE_DATA_DIR 不参与 legacy 迁移，但仍在持锁后验证并创建目录。
            let resolved = Config::try_config_dir()?;
            if resolved != paths.data_dir {
                return Err(format!(
                    "MUSE_DATA_DIR 解析结果发生变化：预期 `{}`，实际 `{}`。",
                    paths.data_dir.display(),
                    resolved.display()
                )
                .into());
            }
            None
        }
    };
    Ok(PreparedDesktopData {
        instance_lock,
        migration,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_paths = resolve_runtime_data_paths().expect("无法解析 Muse 应用数据目录");
    // SAFETY: 在构建 Tauri 运行时和注册插件前只写入一次，后续线程只读。
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &data_paths.data_dir);
    }
    let app = tauri::Builder::default()
        // 单实例插件必须最先注册；迁移只在后续 app setup 中发生，第二次启动不会碰 legacy。
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            if let Some(path) = std::env::var_os(SINGLE_INSTANCE_CI_MARKER_ENV).map(PathBuf::from)
                && let Err(error) = write_single_instance_ci_marker(&path, &args, &cwd)
            {
                tracing::error!(
                    target: "muse::desktop",
                    %error,
                    "写入单实例 CI marker 失败"
                );
            }
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .invoke_handler(tauri::generate_handler![runtime_bootstrap, runtime_ready])
        .setup(move |app| {
            // 插件 setup 已先完成；现在才允许在双锁保护下检查和复制 legacy 数据。
            let prepared = prepare_runtime_data(&data_paths)?;
            if prepared.migration.is_some() {
                eprintln!("旧版工作区数据迁移完成，原目录保持不变。");
            }
            let dev_url = app.config().build.dev_url.clone();
            let started = tauri::async_runtime::block_on(start_local_server(dev_url.clone()))
                .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
            let api_origin = started.bootstrap.api_origin.clone();
            app.manage(DesktopRuntimeState {
                bootstrap: started.bootstrap,
                data_dir: data_paths.data_dir.clone(),
                shutdown: Mutex::new(Some(started.shutdown)),
                _instance_lock: prepared.instance_lock,
            });

            let window_builder = WebviewWindowBuilder::new(
                app,
                MAIN_WINDOW_LABEL,
                WebviewUrl::App("index.html".into()),
            )
            .title("Muse｜角色扮演 Agent")
            .shadow(true)
            .inner_size(1280.0, 820.0)
            .min_inner_size(960.0, 640.0);
            #[cfg(target_os = "macos")]
            let window_builder = window_builder
                .decorations(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            #[cfg(not(target_os = "macos"))]
            let window_builder = window_builder.decorations(false);

            window_builder
                .on_web_resource_request(move |_request, response| {
                    inject_runtime_csp(response, &api_origin);
                })
                .on_navigation(move |url| is_trusted_app_navigation(url, dev_url.as_ref()))
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Muse 桌面窗口初始化失败");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. })
            && let Some(state) = app_handle.try_state::<DesktopRuntimeState>()
            && let Ok(mut shutdown) = state.shutdown.lock()
            && let Some(shutdown) = shutdown.take()
        {
            let _ = shutdown.send(());
        }
    });
}

#[cfg(test)]
mod tests {
    fn unique_temp_root() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "muse-lock-migration-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ))
    }

    #[test]
    fn navigation_allows_only_embedded_app_or_exact_loopback_dev_origin() {
        let dev_url = tauri::Url::parse("http://127.0.0.1:5173").expect("开发 URL 应有效");
        let dev_route = tauri::Url::parse("http://127.0.0.1:5173/#/chat").expect("开发路由应有效");
        assert_eq!(
            super::is_trusted_app_navigation(&dev_route, Some(&dev_url)),
            cfg!(dev)
        );

        let wrong_port =
            tauri::Url::parse("http://127.0.0.1:5174/#/chat").expect("错误端口 URL 应有效");
        let wrong_host =
            tauri::Url::parse("http://localhost:5173/#/chat").expect("错误主机 URL 应有效");
        let external = tauri::Url::parse("https://example.com").expect("外部 URL 应有效");
        assert!(!super::is_trusted_app_navigation(
            &wrong_port,
            Some(&dev_url)
        ));
        assert!(!super::is_trusted_app_navigation(
            &wrong_host,
            Some(&dev_url)
        ));
        assert!(!super::is_trusted_app_navigation(&external, Some(&dev_url)));

        #[cfg(target_os = "macos")]
        let embedded = tauri::Url::parse("tauri://localhost/#/chat").expect("应用 URL 应有效");
        #[cfg(not(target_os = "macos"))]
        let embedded = tauri::Url::parse("http://tauri.localhost/#/chat").expect("应用 URL 应有效");
        assert!(super::is_trusted_app_navigation(&embedded, None));
    }

    #[test]
    fn runtime_csp_preserves_tauri_ipc_and_adds_only_local_api_origins() {
        let mut response = tauri::http::Response::builder()
            .header(
                "Content-Security-Policy",
                "default-src 'self'; connect-src 'self' ipc: http://ipc.localhost",
            )
            .body(std::borrow::Cow::Borrowed(&[] as &'static [u8]))
            .expect("测试响应应有效");

        super::inject_runtime_csp(&mut response, "http://127.0.0.1:43125");

        let policy = response
            .headers()
            .get("Content-Security-Policy")
            .and_then(|value| value.to_str().ok())
            .expect("响应应保留 CSP");
        for source in [
            "'self'",
            "ipc:",
            "http://ipc.localhost",
            "http://127.0.0.1:43125",
            "ws://127.0.0.1:43125",
        ] {
            assert!(policy.contains(source), "CSP 缺少连接源 {source}：{policy}");
        }
        assert!(!policy.contains("https:"), "CSP 不得放宽为任意 HTTPS 来源");
        assert!(!policy.contains("wss:"), "CSP 不得放宽为任意 WSS 来源");
    }

    #[test]
    fn migration_requires_legacy_lock_and_keeps_target_lock_after_commit() {
        let root = unique_temp_root();
        let legacy = root.join("workspace/.agent-vp-data");
        let target = root.join("home/.muse");
        std::fs::create_dir_all(&legacy).expect("应能创建 legacy 测试目录");
        std::fs::write(legacy.join("legacy.txt"), b"legacy").expect("应能写入 legacy 测试文件");
        let paths = super::DesktopDataPaths {
            data_dir: target.clone(),
            legacy_dir: Some(legacy.clone()),
        };

        let source_blocker = muse_core::config::Config::acquire_data_dir_lock(&legacy)
            .expect("应能预占 legacy 源锁");
        assert!(
            super::prepare_runtime_data(&paths).is_err(),
            "未取得 legacy 源锁时不得开始迁移"
        );
        drop(source_blocker);

        let prepared = super::prepare_runtime_data(&paths).expect("双锁可用时迁移应成功");
        assert_eq!(
            std::fs::read(target.join("legacy.txt")).expect("迁移文件应存在"),
            b"legacy"
        );
        assert!(
            muse_core::config::Config::acquire_data_dir_lock(&target).is_err(),
            "提交后目标锁应继续由桌面状态持有"
        );

        drop(prepared);
        std::fs::remove_dir_all(root).expect("应能清理迁移测试目录");
    }

    #[test]
    fn legacy_discovery_accepts_only_workspace_root() {
        let root = unique_temp_root();
        let nested = root.join("src-tauri");
        std::fs::create_dir_all(&nested).expect("应创建 Tauri 目录");
        std::fs::create_dir_all(root.join(".agent-vp-data")).expect("应创建根 legacy 目录");
        std::fs::create_dir_all(nested.join(".agent-vp-data")).expect("应创建嵌套 legacy 目录");
        std::fs::write(root.join("Cargo.toml"), "[workspace]").expect("应写入 workspace 标识");
        std::fs::write(root.join("src-tauri/Cargo.toml"), "[package]")
            .expect("应写入 Tauri crate 标识");
        std::fs::write(root.join("package.json"), "{}").expect("应写入前端标识");

        assert_eq!(
            super::recognized_legacy_workspace_dir(&root),
            Some(root.join(".agent-vp-data"))
        );
        assert_eq!(super::recognized_legacy_workspace_dir(&nested), None);
        std::fs::remove_dir_all(root).expect("应清理 legacy 发现测试目录");
    }

    #[test]
    fn desktop_ready_file_is_idempotent_and_contains_no_bearer() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("应创建就绪测试目录");
        let path = root.join("ready.json");
        let data_dir = root.join("app-data");

        super::write_desktop_ready_file(&path, "muse-local-api/v1", "instance-test", &data_dir)
            .expect("首次就绪证明应写入");
        super::write_desktop_ready_file(&path, "muse-local-api/v1", "instance-test", &data_dir)
            .expect("相同实例重复报告应幂等");

        let content = std::fs::read_to_string(&path).expect("应读取就绪证明");
        let value: serde_json::Value = serde_json::from_str(&content).expect("就绪证明应为 JSON");
        assert_eq!(value["protocol_version"], "muse-local-api/v1");
        assert_eq!(value["instance_id"], "instance-test");
        assert!(value.get("access_token").is_none());
        assert!(value.get("token").is_none());

        assert!(
            super::write_desktop_ready_file(
                &path,
                "muse-local-api/v1",
                "other-instance",
                &data_dir,
            )
            .is_err(),
            "其他实例不能覆盖已有就绪证明"
        );
        std::fs::remove_dir_all(root).expect("应清理就绪测试目录");
    }

    #[test]
    fn single_instance_ci_marker_is_create_new_private_and_redacts_secrets() {
        let root = unique_temp_root();
        std::fs::create_dir_all(&root).expect("应创建单实例 marker 测试目录");
        assert!(
            super::write_single_instance_ci_marker(
                std::path::Path::new("relative-marker.json"),
                &[],
                "/tmp/cwd-two",
            )
            .is_err(),
            "CI marker 不得接受相对路径"
        );
        let marker = root.join("single-instance.json");
        let args = vec![
            "/Applications/Muse.app/Contents/MacOS/Muse".to_string(),
            "--muse-ci-secondary-probe".to_string(),
            "--api-token".to_string(),
            "不得写入-marker".to_string(),
            "--authorization=Bearer-不得写入".to_string(),
        ];

        super::write_single_instance_ci_marker(&marker, &args, "/tmp/cwd-two")
            .expect("首次单实例 marker 应写入");
        assert!(
            super::write_single_instance_ci_marker(&marker, &args, "/tmp/cwd-two").is_err(),
            "marker 必须使用 create_new，不能被覆盖"
        );

        let content = std::fs::read_to_string(&marker).expect("应读取单实例 marker");
        let value: serde_json::Value = serde_json::from_str(&content).expect("marker 应为 JSON");
        assert_eq!(
            value["schema_version"],
            super::SINGLE_INSTANCE_CI_MARKER_SCHEMA
        );
        assert_eq!(value["cwd"], "/tmp/cwd-two");
        assert_eq!(value["args"][1], "--muse-ci-secondary-probe");
        assert!(!content.contains("不得写入-marker"));
        assert!(!content.contains("Bearer-不得写入"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&marker)
                .expect("应读取 marker 权限")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);

            let linked_parent = root.join("linked-parent");
            std::os::unix::fs::symlink(&root, &linked_parent).expect("应创建父目录符号链接");
            assert!(
                super::write_single_instance_ci_marker(
                    &linked_parent.join("unexpected.json"),
                    &[],
                    "/tmp/cwd-two",
                )
                .is_err(),
                "CI marker 不得写入符号链接父目录"
            );
            std::fs::remove_file(linked_parent).expect("应清理父目录符号链接");
        }
        std::fs::remove_dir_all(root).expect("应清理单实例 marker 测试目录");
    }
}
