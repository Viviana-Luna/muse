//! 统一 MCP client/session 生命周期。
//!
//! manager 只保留弱引用；真正的 session 由目录快照 lease 持有。配置 revision
//! 改变后，新 Turn 获取新 session，旧 Turn 释放最后一个 lease 时再渐进关闭旧连接。

use super::{
    ExternalMcpServerConfig, ExternalMcpTransport, MCP_COMPATIBLE_PROTOCOL_VERSION,
    MCP_PROTOCOL_VERSION,
};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";
const HTTP_SSE_BUFFER_BYTES: usize = 1024 * 1024;
const STDERR_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const STDIO_CLOSE_GRACE: Duration = Duration::from_millis(500);
const STDIO_TERM_GRACE: Duration = Duration::from_millis(500);
const STDIO_KILL_GRACE: Duration = Duration::from_secs(2);

/// 初始化后冻结的 MCP 协商结果。
#[derive(Debug, Clone, PartialEq)]
pub struct McpNegotiatedSession {
    pub protocol_version: String,
    pub server_capabilities: Value,
    pub server_info: Value,
    pub instructions: Option<String>,
}

/// 进程内统一 MCP client/session manager。
#[derive(Clone, Default)]
pub struct McpClientManager {
    inner: Arc<McpClientManagerInner>,
}

#[derive(Default)]
struct McpClientManagerInner {
    sessions: StdMutex<BTreeMap<McpClientKey, Weak<McpClientSession>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct McpClientKey {
    server_name: String,
    config_revision: String,
}

impl McpClientManager {
    pub(super) async fn lease(
        &self,
        server: &ExternalMcpServerConfig,
    ) -> Result<McpClientLease, String> {
        let key = McpClientKey {
            server_name: server.name.clone(),
            config_revision: server.config_revision.clone(),
        };
        if let Some(session) = lock_std(&self.inner.sessions)
            .get(&key)
            .and_then(Weak::upgrade)
            .filter(|session| session.is_healthy())
        {
            return Ok(McpClientLease { session });
        }

        let created = McpClientSession::connect(server).await?;
        let mut sessions = lock_std(&self.inner.sessions);
        if let Some(existing) = sessions
            .get(&key)
            .and_then(Weak::upgrade)
            .filter(|session| session.is_healthy())
        {
            drop(created);
            return Ok(McpClientLease { session: existing });
        }
        sessions.retain(|_, session| session.strong_count() > 0);
        sessions.insert(key, Arc::downgrade(&created));
        Ok(McpClientLease { session: created })
    }

    /// 应用退出时请求所有仍存活的 session 开始关闭；进程树守卫负责最终兜底。
    pub fn request_close_all(&self) {
        let sessions = lock_std(&self.inner.sessions);
        for session in sessions.values().filter_map(Weak::upgrade) {
            session.request_close();
        }
    }
}

/// 一个目录快照持有的 MCP session lease。
#[derive(Clone)]
pub(super) struct McpClientLease {
    session: Arc<McpClientSession>,
}

impl std::fmt::Debug for McpClientLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClientLease")
            .field("server_name", &self.server_name())
            .field("config_revision", &self.config_revision())
            .field("healthy", &self.is_healthy())
            .finish()
    }
}

impl McpClientLease {
    pub(crate) fn server_name(&self) -> &str {
        &self.session.server_name
    }

    pub(crate) fn config_revision(&self) -> &str {
        &self.session.config_revision
    }

    pub(crate) fn catalog_epoch(&self) -> u64 {
        self.session.events.catalog_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.session.is_healthy()
    }

    pub(crate) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        self.session.request(method, params).await
    }
}

struct McpClientSession {
    server_name: String,
    config_revision: String,
    timeout: Duration,
    next_request_id: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    events: Arc<McpSessionEvents>,
    negotiated: Arc<RwLock<McpNegotiatedSession>>,
    transport: ManagedTransport,
}

impl McpClientSession {
    async fn connect(server: &ExternalMcpServerConfig) -> Result<Arc<Self>, String> {
        let next_request_id = Arc::new(AtomicU64::new(1));
        let healthy = Arc::new(AtomicBool::new(true));
        let events = Arc::new(McpSessionEvents::default());
        let placeholder = McpNegotiatedSession {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            server_capabilities: serde_json::json!({}),
            server_info: serde_json::json!({}),
            instructions: None,
        };
        let negotiated = Arc::new(RwLock::new(placeholder));
        let timeout = Duration::from_millis(server.timeout_ms());
        let transport = match &server.transport {
            ExternalMcpTransport::Stdio { .. } => ManagedTransport::Stdio(
                StdioClientSession::spawn(server, Arc::clone(&healthy), Arc::clone(&events))
                    .await?,
            ),
            ExternalMcpTransport::StreamableHttp { .. } => {
                ManagedTransport::Http(HttpClientSession::new(
                    server,
                    Arc::clone(&next_request_id),
                    Arc::clone(&healthy),
                    Arc::clone(&events),
                    Arc::clone(&negotiated),
                )?)
            }
        };
        let session = Arc::new(Self {
            server_name: server.name.clone(),
            config_revision: server.config_revision.clone(),
            timeout,
            next_request_id,
            healthy,
            events,
            negotiated,
            transport,
        });
        let initialization = session.transport.initialize(&session).await;
        match initialization {
            Ok(value) => {
                *session.negotiated.write().await = value;
                Ok(session)
            }
            Err(error) => {
                session.healthy.store(false, Ordering::Release);
                session.request_close();
                Err(error.message)
            }
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && self.transport.is_open()
    }

    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        if !self.is_healthy() {
            return Err(format!(
                "外部 MCP server `{}` 的连接已经失效，请在新回合重新连接。",
                self.server_name
            ));
        }
        self.ensure_capability(method).await?;
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let result = self
            .transport
            .request(id, method, params, self.timeout, true)
            .await;
        if let Err(error) = &result
            && error.fatal
        {
            self.healthy.store(false, Ordering::Release);
            self.request_close();
        }
        result.map_err(|error| error.message)
    }

    async fn ensure_capability(&self, method: &str) -> Result<(), String> {
        let capability = if method.starts_with("tools/") {
            Some("tools")
        } else if method.starts_with("resources/") {
            Some("resources")
        } else {
            None
        };
        let Some(capability) = capability else {
            return Ok(());
        };
        let negotiated = self.negotiated.read().await;
        if negotiated.server_capabilities.get(capability).is_some() {
            return Ok(());
        }
        Err(format!(
            "外部 MCP server `{}` 未声明 `{capability}` capability，不能调用 `{method}`。",
            self.server_name
        ))
    }

    fn request_close(&self) {
        self.transport.request_close();
    }
}

impl Drop for McpClientSession {
    fn drop(&mut self) {
        self.request_close();
    }
}

#[derive(Default)]
struct McpSessionEvents {
    catalog_epoch: AtomicU64,
    notification_count: AtomicU64,
}

impl McpSessionEvents {
    fn observe_notification(&self, method: &str) {
        self.notification_count.fetch_add(1, Ordering::Relaxed);
        if matches!(
            method,
            "notifications/tools/list_changed"
                | "notifications/resources/list_changed"
                | "notifications/prompts/list_changed"
        ) {
            self.catalog_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
}

enum ManagedTransport {
    Stdio(Arc<StdioClientSession>),
    Http(Arc<HttpClientSession>),
}

impl ManagedTransport {
    async fn initialize(
        &self,
        session: &McpClientSession,
    ) -> Result<McpNegotiatedSession, McpSessionError> {
        match self {
            Self::Stdio(transport) => {
                let id = session.next_request_id.fetch_add(1, Ordering::Relaxed);
                let result = transport
                    .request(
                        id,
                        "initialize",
                        Some(initialize_params()),
                        session.timeout,
                        false,
                    )
                    .await?;
                let negotiated = validate_initialize_result(&session.server_name, result)?;
                transport
                    .notification("notifications/initialized", None)
                    .map_err(McpSessionError::fatal)?;
                Ok(negotiated)
            }
            Self::Http(transport) => transport.initialize().await,
        }
    }

    async fn request(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancellable: bool,
    ) -> Result<Value, McpSessionError> {
        match self {
            Self::Stdio(transport) => {
                transport
                    .request(id, method, params, timeout, cancellable)
                    .await
            }
            Self::Http(transport) => {
                transport
                    .request(id, method, params, timeout, cancellable)
                    .await
            }
        }
    }

    fn is_open(&self) -> bool {
        match self {
            Self::Stdio(transport) => transport.is_open(),
            Self::Http(transport) => transport.is_open(),
        }
    }

    fn request_close(&self) {
        match self {
            Self::Stdio(transport) => transport.request_close(),
            Self::Http(transport) => transport.request_close(),
        }
    }
}

#[derive(Debug, Clone)]
struct McpSessionError {
    message: String,
    fatal: bool,
    status: Option<StatusCode>,
}

impl McpSessionError {
    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
            status: None,
        }
    }

    fn server(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
            status: None,
        }
    }
}

type PendingResponse = oneshot::Sender<Result<Value, McpSessionError>>;
type PendingResponses = Arc<StdMutex<BTreeMap<u64, PendingResponse>>>;

enum StdioWriterCommand {
    Message(Value),
    Close(oneshot::Sender<()>),
}

struct StdioClientSession {
    server_name: String,
    writer: mpsc::UnboundedSender<StdioWriterCommand>,
    pending: PendingResponses,
    close: StdMutex<Option<oneshot::Sender<()>>>,
    healthy: Arc<AtomicBool>,
}

impl StdioClientSession {
    async fn spawn(
        server: &ExternalMcpServerConfig,
        healthy: Arc<AtomicBool>,
        events: Arc<McpSessionEvents>,
    ) -> Result<Arc<Self>, String> {
        let ExternalMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } = &server.transport
        else {
            return Err("MCP stdio session 收到了非 stdio 配置。".to_string());
        };
        let mut command_builder = Command::new(command);
        command_builder
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::process_supervision::configure_process_tree(&mut command_builder);
        for (key, value) in env {
            command_builder.env(key, value);
        }
        if let Some(cwd) = cwd {
            command_builder.current_dir(cwd);
        }
        let mut child = command_builder
            .spawn()
            .map_err(|error| format!("启动外部 MCP server `{}` 失败：{error}", server.name))?;
        let process_tree_guard = match crate::process_supervision::ProcessTreeGuard::attach(&child)
        {
            Ok(guard) => guard,
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(format!(
                    "外部 MCP server `{}` 进程树隔离失败：{error}",
                    server.name
                ));
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("外部 MCP server `{}` 无法打开 stdin。", server.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("外部 MCP server `{}` 无法打开 stdout。", server.name))?;
        let stderr = child.stderr.take();
        let pending = Arc::new(StdMutex::new(BTreeMap::new()));
        let diagnostic = Arc::new(BoundedDiagnostic::new(server.diagnostic_redactions()));
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let (close_tx, close_rx) = oneshot::channel();
        let (failure_tx, failure_rx) = mpsc::unbounded_channel();

        let writer_task = tokio::spawn(run_stdio_writer(stdin, writer_rx));
        let reader_task = tokio::spawn(run_stdio_reader(
            server.name.clone(),
            stdout,
            Arc::clone(&pending),
            writer_tx.clone(),
            failure_tx,
            Arc::clone(&events),
        ));
        let stderr_task = tokio::spawn(drain_stderr(stderr, Arc::clone(&diagnostic)));
        tokio::spawn(supervise_stdio_process(
            server.name.clone(),
            child,
            process_tree_guard,
            writer_tx.clone(),
            Arc::clone(&pending),
            Arc::clone(&healthy),
            diagnostic,
            close_rx,
            failure_rx,
            writer_task,
            reader_task,
            stderr_task,
        ));

        Ok(Arc::new(Self {
            server_name: server.name.clone(),
            writer: writer_tx,
            pending,
            close: StdMutex::new(Some(close_tx)),
            healthy,
        }))
    }

    async fn request(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancellable: bool,
    ) -> Result<Value, McpSessionError> {
        if !self.is_open() {
            return Err(McpSessionError::fatal(format!(
                "外部 MCP server `{}` 的 stdio 已关闭。",
                self.server_name
            )));
        }
        let (sender, receiver) = oneshot::channel();
        lock_pending(&self.pending).insert(id, sender);
        let mut guard = StdioPendingGuard {
            id,
            pending: Arc::clone(&self.pending),
            writer: self.writer.clone(),
            cancellable,
            armed: true,
        };
        if self
            .writer
            .send(StdioWriterCommand::Message(json_rpc_request(
                id, method, params,
            )))
            .is_err()
        {
            guard.cancellable = false;
            return Err(McpSessionError::fatal(format!(
                "外部 MCP server `{}` 的 stdin 已关闭。",
                self.server_name
            )));
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => {
                guard.armed = false;
                lock_pending(&self.pending).remove(&id);
                result
            }
            Ok(Err(_)) => Err(McpSessionError::fatal(format!(
                "外部 MCP server `{}` 的响应通道已关闭。",
                self.server_name
            ))),
            Err(_) => Err(McpSessionError::fatal(format!(
                "外部 MCP server `{}` 请求 `{method}` 超时。",
                self.server_name
            ))),
        }
    }

    fn notification(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        self.writer
            .send(StdioWriterCommand::Message(json_rpc_notification(
                method, params,
            )))
            .map_err(|_| format!("外部 MCP server `{}` 的 stdin 已关闭。", self.server_name))
    }

    fn is_open(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
            && lock_std(&self.close).as_ref().is_some()
            && !self.writer.is_closed()
    }

    fn request_close(&self) {
        if let Some(close) = lock_std(&self.close).take() {
            let _ = close.send(());
        }
    }
}

struct StdioPendingGuard {
    id: u64,
    pending: PendingResponses,
    writer: mpsc::UnboundedSender<StdioWriterCommand>,
    cancellable: bool,
    armed: bool,
}

impl Drop for StdioPendingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        lock_pending(&self.pending).remove(&self.id);
        if self.cancellable {
            let _ = self
                .writer
                .send(StdioWriterCommand::Message(cancellation_notification(
                    self.id,
                    "请求 Future 已取消或超时",
                )));
        }
    }
}

async fn run_stdio_writer(
    mut stdin: tokio::process::ChildStdin,
    mut receiver: mpsc::UnboundedReceiver<StdioWriterCommand>,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            StdioWriterCommand::Message(value) => {
                let Ok(mut line) = serde_json::to_vec(&value) else {
                    continue;
                };
                line.push(b'\n');
                if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            StdioWriterCommand::Close(acknowledge) => {
                let _ = stdin.shutdown().await;
                let _ = acknowledge.send(());
                break;
            }
        }
    }
}

async fn run_stdio_reader(
    server_name: String,
    stdout: tokio::process::ChildStdout,
    pending: PendingResponses,
    writer: mpsc::UnboundedSender<StdioWriterCommand>,
    failure: mpsc::UnboundedSender<String>,
    events: Arc<McpSessionEvents>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => continue,
            Ok(Some(line)) => {
                let value: Value = match serde_json::from_str::<Value>(&line) {
                    Ok(value) if value.is_object() => value,
                    Ok(_) => {
                        let _ = failure.send(format!(
                            "外部 MCP server `{server_name}` stdout 返回了非对象 JSON-RPC 消息。"
                        ));
                        break;
                    }
                    Err(error) => {
                        let _ = failure.send(format!(
                            "解析外部 MCP server `{server_name}` stdout 失败：{error}"
                        ));
                        break;
                    }
                };
                dispatch_incoming_message(&value, &pending, &writer, &events);
            }
            Ok(None) => {
                let _ = failure.send(format!("外部 MCP server `{server_name}` 已关闭 stdout。"));
                break;
            }
            Err(error) => {
                let _ = failure.send(format!(
                    "读取外部 MCP server `{server_name}` stdout 失败：{error}"
                ));
                break;
            }
        }
    }
}

fn dispatch_incoming_message(
    value: &Value,
    pending: &PendingResponses,
    writer: &mpsc::UnboundedSender<StdioWriterCommand>,
    events: &McpSessionEvents,
) {
    if value.get("method").is_some() {
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            if value.get("id").is_none() {
                events.observe_notification(method);
            } else if let Some(response) = response_for_server_request(value) {
                let _ = writer.send(StdioWriterCommand::Message(response));
            }
        }
        return;
    }
    let Some(id) = json_rpc_id(value.get("id")) else {
        return;
    };
    let Some(sender) = lock_pending(pending).remove(&id) else {
        return;
    };
    let _ = sender.send(extract_response_result(value));
}

#[allow(clippy::too_many_arguments)]
async fn supervise_stdio_process(
    server_name: String,
    mut child: tokio::process::Child,
    process_tree_guard: crate::process_supervision::ProcessTreeGuard,
    writer: mpsc::UnboundedSender<StdioWriterCommand>,
    pending: PendingResponses,
    healthy: Arc<AtomicBool>,
    diagnostic: Arc<BoundedDiagnostic>,
    close: oneshot::Receiver<()>,
    mut failures: mpsc::UnboundedReceiver<String>,
    writer_task: tokio::task::JoinHandle<()>,
    reader_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
) {
    let failure_message = tokio::select! {
        _ = close => None,
        failure = failures.recv() => failure,
        result = child.wait() => {
            let message = match result {
                Ok(status) => format!("外部 MCP server `{server_name}` 已退出：{status}。"),
                Err(error) => format!("等待外部 MCP server `{server_name}` 退出失败：{error}"),
            };
            fail_all_pending(&pending, McpSessionError::fatal(with_diagnostic(message, &diagnostic)));
            healthy.store(false, Ordering::Release);
            writer_task.abort();
            reader_task.abort();
            stderr_task.abort();
            return;
        }
    };

    if let Some(message) = failure_message {
        fail_all_pending(
            &pending,
            McpSessionError::fatal(with_diagnostic(message, &diagnostic)),
        );
    }
    healthy.store(false, Ordering::Release);
    let (acknowledge, acknowledged) = oneshot::channel();
    let _ = writer.send(StdioWriterCommand::Close(acknowledge));
    let _ = tokio::time::timeout(Duration::from_millis(200), acknowledged).await;

    let exited = tokio::time::timeout(STDIO_CLOSE_GRACE, child.wait())
        .await
        .is_ok();
    if !exited {
        let _ = process_tree_guard.terminate(false);
        let exited_after_term = tokio::time::timeout(STDIO_TERM_GRACE, child.wait())
            .await
            .is_ok();
        if !exited_after_term {
            let _ = process_tree_guard.terminate(true);
            let _ = tokio::time::timeout(STDIO_KILL_GRACE, child.wait()).await;
        }
    }
    // 不解除进程树守卫：根进程正常退出也不能证明其后代已经退出。守卫 Drop 会
    // 对整个进程组或 Job Object 做最终回收，已退出时系统会安全返回不存在。
    writer_task.abort();
    reader_task.abort();
    stderr_task.abort();
}

struct BoundedDiagnostic {
    content: StdMutex<String>,
    redactions: Vec<String>,
}

impl BoundedDiagnostic {
    fn new(redactions: Vec<String>) -> Self {
        Self {
            content: StdMutex::new(String::new()),
            redactions,
        }
    }

    fn push(&self, text: &str) {
        let mut redacted = text.to_string();
        for value in &self.redactions {
            if !value.is_empty() {
                redacted = redacted.replace(value, "[已脱敏]");
            }
        }
        let mut content = lock_std(&self.content);
        content.push_str(&redacted);
        if content.len() > STDERR_DIAGNOSTIC_BYTES {
            let mut start = content.len() - STDERR_DIAGNOSTIC_BYTES;
            while !content.is_char_boundary(start) {
                start += 1;
            }
            content.drain(..start);
        }
    }

    fn summary(&self) -> String {
        lock_std(&self.content).trim().to_string()
    }
}

async fn drain_stderr(
    stderr: Option<tokio::process::ChildStderr>,
    diagnostic: Arc<BoundedDiagnostic>,
) {
    let Some(mut stderr) = stderr else {
        return;
    };
    let mut buffer = [0_u8; 4_096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => diagnostic.push(&String::from_utf8_lossy(&buffer[..read])),
        }
    }
}

fn with_diagnostic(message: String, diagnostic: &BoundedDiagnostic) -> String {
    let summary = diagnostic.summary();
    if summary.is_empty() {
        message
    } else {
        format!("{message}\nstderr 摘要：{summary}")
    }
}

struct HttpClientSession {
    server_name: String,
    url: String,
    headers: HeaderMap,
    client: reqwest::Client,
    timeout: Duration,
    next_request_id: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    events: Arc<McpSessionEvents>,
    negotiated: Arc<RwLock<McpNegotiatedSession>>,
    state: Mutex<HttpConnectionState>,
    receiver_generation: AtomicU64,
    notification_receiver: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    closed: AtomicBool,
}

#[derive(Default)]
struct HttpConnectionState {
    protocol_version: Option<String>,
    session_id: Option<String>,
}

impl HttpClientSession {
    fn new(
        server: &ExternalMcpServerConfig,
        next_request_id: Arc<AtomicU64>,
        healthy: Arc<AtomicBool>,
        events: Arc<McpSessionEvents>,
        negotiated: Arc<RwLock<McpNegotiatedSession>>,
    ) -> Result<Arc<Self>, String> {
        let ExternalMcpTransport::StreamableHttp { url, headers } = &server.transport else {
            return Err("MCP HTTP session 收到了非 HTTP 配置。".to_string());
        };
        Ok(Arc::new(Self {
            server_name: server.name.clone(),
            url: url.clone(),
            headers: build_http_headers(&server.name, headers)?,
            client: reqwest::Client::new(),
            timeout: Duration::from_millis(server.timeout_ms()),
            next_request_id,
            healthy,
            events,
            negotiated,
            state: Mutex::new(HttpConnectionState::default()),
            receiver_generation: AtomicU64::new(0),
            notification_receiver: StdMutex::new(None),
            closed: AtomicBool::new(false),
        }))
    }

    async fn initialize(self: &Arc<Self>) -> Result<McpNegotiatedSession, McpSessionError> {
        let mut state = self.state.lock().await;
        self.initialize_locked(&mut state).await
    }

    async fn initialize_locked(
        self: &Arc<Self>,
        state: &mut HttpConnectionState,
    ) -> Result<McpNegotiatedSession, McpSessionError> {
        state.protocol_version = None;
        state.session_id = None;
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .post_message(
                json_rpc_request(id, "initialize", Some(initialize_params())),
                None,
                None,
                self.timeout,
            )
            .await?;
        let parsed = parse_http_response(&response.text, id)?;
        self.handle_side_messages(&parsed.side_messages, state)
            .await?;
        let result = parsed.response.ok_or_else(|| {
            McpSessionError::fatal("MCP initialize 响应中没有匹配请求 id 的结果。")
        })??;
        let negotiated = validate_initialize_result(&self.server_name, result)?;
        validate_session_id(&self.server_name, response.session_id.as_deref())?;
        state.protocol_version = Some(negotiated.protocol_version.clone());
        state.session_id = response.session_id;
        self.post_message(
            json_rpc_notification("notifications/initialized", None),
            state.session_id.as_deref(),
            state.protocol_version.as_deref(),
            self.timeout,
        )
        .await?;
        *self.negotiated.write().await = negotiated.clone();
        self.start_notification_receiver(state.session_id.clone(), state.protocol_version.clone());
        Ok(negotiated)
    }

    async fn request(
        self: &Arc<Self>,
        id: u64,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancellable: bool,
    ) -> Result<Value, McpSessionError> {
        let mut state = self.state.lock().await;
        if state.protocol_version.is_none() {
            self.initialize_locked(&mut state).await?;
        }
        let mut cancellation = HttpCancellationGuard {
            session: Arc::downgrade(self),
            request_id: id,
            armed: cancellable,
        };
        let payload = json_rpc_request(id, method, params);
        let result = async {
            let mut response = self
                .post_message(
                    payload.clone(),
                    state.session_id.as_deref(),
                    state.protocol_version.as_deref(),
                    timeout,
                )
                .await;
            if response.as_ref().err().and_then(|error| error.status) == Some(StatusCode::NOT_FOUND)
                && state.session_id.is_some()
            {
                self.initialize_locked(&mut state).await?;
                response = self
                    .post_message(
                        payload,
                        state.session_id.as_deref(),
                        state.protocol_version.as_deref(),
                        timeout,
                    )
                    .await;
            }
            let response = response?;
            let parsed = parse_http_response(&response.text, id)?;
            self.handle_side_messages(&parsed.side_messages, &state)
                .await?;
            parsed.response.ok_or_else(|| {
                McpSessionError::fatal(format!(
                    "外部 MCP server `{}` 的 HTTP 响应中没有匹配请求 id。",
                    self.server_name
                ))
            })?
        }
        .await;
        if cancellation.armed && result.as_ref().is_err_and(|error| error.fatal) {
            self.send_cancellation_with_state(id, &state).await;
        }
        cancellation.armed = false;
        result
    }

    async fn handle_side_messages(
        &self,
        messages: &[Value],
        state: &HttpConnectionState,
    ) -> Result<(), McpSessionError> {
        for value in messages {
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                continue;
            };
            if value.get("id").is_none() {
                self.events.observe_notification(method);
            } else if let Some(response) = response_for_server_request(value) {
                self.post_message(
                    response,
                    state.session_id.as_deref(),
                    state.protocol_version.as_deref(),
                    self.timeout,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn post_message(
        &self,
        payload: Value,
        session_id: Option<&str>,
        protocol_version: Option<&str>,
        timeout: Duration,
    ) -> Result<HttpMessageResponse, McpSessionError> {
        let mut headers = self.headers.clone();
        if let Some(session_id) = session_id {
            headers.insert(
                HeaderName::from_static(MCP_SESSION_ID_HEADER),
                HeaderValue::from_str(session_id).map_err(|error| {
                    McpSessionError::fatal(format!(
                        "外部 MCP server `{}` 的 session id 无法写入 Header：{error}",
                        self.server_name
                    ))
                })?,
            );
        }
        if let Some(protocol_version) = protocol_version {
            headers.insert(
                HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER),
                HeaderValue::from_str(protocol_version).map_err(|error| {
                    McpSessionError::fatal(format!(
                        "外部 MCP server `{}` 的协议版本无法写入 Header：{error}",
                        self.server_name
                    ))
                })?,
            );
        }
        let response = self
            .client
            .post(&self.url)
            .headers(headers)
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                McpSessionError::fatal(if error.is_timeout() {
                    format!("外部 MCP server `{}` 请求超时。", self.server_name)
                } else {
                    format!("请求外部 MCP server `{}` 失败：{error}", self.server_name)
                })
            })?;
        let status = response.status();
        let session_id = response
            .headers()
            .get(HeaderName::from_static(MCP_SESSION_ID_HEADER))
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let text = response.text().await.map_err(|error| {
            McpSessionError::fatal(format!(
                "读取外部 MCP server `{}` 响应失败：{error}",
                self.server_name
            ))
        })?;
        if !status.is_success() {
            return Err(McpSessionError {
                message: format!(
                    "外部 MCP server `{}` 返回 HTTP {status}：{}",
                    self.server_name,
                    truncate_error_text(&text)
                ),
                fatal: status == StatusCode::NOT_FOUND || status.is_server_error(),
                status: Some(status),
            });
        }
        Ok(HttpMessageResponse { text, session_id })
    }

    async fn send_cancellation(&self, request_id: u64) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let state = self.state.lock().await;
        self.send_cancellation_with_state(request_id, &state).await;
    }

    async fn send_cancellation_with_state(&self, request_id: u64, state: &HttpConnectionState) {
        let _ = self
            .post_message(
                cancellation_notification(request_id, "调用方已停止等待"),
                state.session_id.as_deref(),
                state.protocol_version.as_deref(),
                Duration::from_secs(5),
            )
            .await;
    }

    fn start_notification_receiver(
        self: &Arc<Self>,
        session_id: Option<String>,
        protocol_version: Option<String>,
    ) {
        let previous = { lock_std(&self.notification_receiver).take() };
        if let Some(previous) = previous {
            previous.abort();
        }
        let generation = self.receiver_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let session = Arc::clone(self);
        let receiver = tokio::spawn(async move {
            session
                .run_notification_receiver(generation, session_id, protocol_version)
                .await;
        });
        *lock_std(&self.notification_receiver) = Some(receiver);
    }

    async fn run_notification_receiver(
        &self,
        generation: u64,
        session_id: Option<String>,
        protocol_version: Option<String>,
    ) {
        let mut headers = self.headers.clone();
        headers.remove(CONTENT_TYPE);
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(session_id) = session_id.as_deref()
            && let Ok(value) = HeaderValue::from_str(session_id)
        {
            headers.insert(HeaderName::from_static(MCP_SESSION_ID_HEADER), value);
        }
        if let Some(protocol_version) = protocol_version.as_deref()
            && let Ok(value) = HeaderValue::from_str(protocol_version)
        {
            headers.insert(HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER), value);
        }
        let Ok(mut response) = self.client.get(&self.url).headers(headers).send().await else {
            return;
        };
        if response.status() == StatusCode::NOT_FOUND {
            self.invalidate_http_session(generation).await;
            return;
        }
        if !response.status().is_success() {
            return;
        }

        let mut buffer = Vec::new();
        loop {
            if self.closed.load(Ordering::Acquire)
                || self.receiver_generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some((frame, consumed)) = next_sse_frame(&buffer) {
                        buffer.drain(..consumed);
                        if let Ok(messages) = parse_sse_messages(&frame) {
                            for message in messages {
                                self.handle_stream_message(
                                    &message,
                                    session_id.as_deref(),
                                    protocol_version.as_deref(),
                                )
                                .await;
                            }
                        }
                    }
                    if buffer.len() > HTTP_SSE_BUFFER_BYTES {
                        tracing::warn!(
                            server = %self.server_name,
                            "MCP HTTP SSE 单条事件超过缓冲上限，停止独立通知接收器"
                        );
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        }
    }

    async fn handle_stream_message(
        &self,
        value: &Value,
        session_id: Option<&str>,
        protocol_version: Option<&str>,
    ) {
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return;
        };
        if value.get("id").is_none() {
            self.events.observe_notification(method);
        } else if let Some(response) = response_for_server_request(value) {
            let _ = self
                .post_message(response, session_id, protocol_version, self.timeout)
                .await;
        }
    }

    async fn invalidate_http_session(&self, generation: u64) {
        if self.receiver_generation.load(Ordering::Acquire) != generation {
            return;
        }
        let mut state = self.state.lock().await;
        if self.receiver_generation.load(Ordering::Acquire) == generation {
            state.session_id = None;
            state.protocol_version = None;
        }
    }

    fn is_open(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && !self.closed.load(Ordering::Acquire)
    }

    fn request_close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.receiver_generation.fetch_add(1, Ordering::AcqRel);
        if let Some(receiver) = lock_std(&self.notification_receiver).take() {
            receiver.abort();
        }
        let session = Arc::clone(self);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                session.close_http_session().await;
            });
        }
    }

    async fn close_http_session(&self) {
        let mut state = self.state.lock().await;
        let Some(session_id) = state.session_id.take() else {
            state.protocol_version = None;
            return;
        };
        let mut headers = self.headers.clone();
        if let Ok(value) = HeaderValue::from_str(&session_id) {
            headers.insert(HeaderName::from_static(MCP_SESSION_ID_HEADER), value);
        }
        if let Some(protocol_version) = state.protocol_version.take()
            && let Ok(value) = HeaderValue::from_str(&protocol_version)
        {
            headers.insert(HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER), value);
        }
        let result = self
            .client
            .delete(&self.url)
            .headers(headers)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        if let Ok(response) = result
            && !response.status().is_success()
            && response.status() != StatusCode::METHOD_NOT_ALLOWED
            && response.status() != StatusCode::NOT_FOUND
        {
            tracing::warn!(
                server = %self.server_name,
                status = %response.status(),
                "关闭 MCP HTTP session 时服务返回非成功状态"
            );
        }
    }
}

struct HttpCancellationGuard {
    session: Weak<HttpClientSession>,
    request_id: u64,
    armed: bool,
}

impl Drop for HttpCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let request_id = self.request_id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                session.send_cancellation(request_id).await;
            });
        }
    }
}

struct HttpMessageResponse {
    text: String,
    session_id: Option<String>,
}

struct ParsedHttpResponse {
    response: Option<Result<Value, McpSessionError>>,
    side_messages: Vec<Value>,
}

fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {
            "name": "muse-mcp-client",
            "version": env!("CARGO_PKG_VERSION"),
            "title": "Muse"
        }
    })
}

fn validate_initialize_result(
    server_name: &str,
    result: Value,
) -> Result<McpNegotiatedSession, McpSessionError> {
    let protocol_version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            McpSessionError::fatal(format!(
                "外部 MCP server `{server_name}` 的 initialize 结果缺少 protocolVersion。"
            ))
        })?;
    if !matches!(
        protocol_version,
        MCP_PROTOCOL_VERSION | MCP_COMPATIBLE_PROTOCOL_VERSION
    ) {
        return Err(McpSessionError::fatal(format!(
            "外部 MCP server `{server_name}` 返回不兼容协议版本 `{protocol_version}`；Muse 当前支持 `{MCP_PROTOCOL_VERSION}` 与 `{MCP_COMPATIBLE_PROTOCOL_VERSION}`。"
        )));
    }
    let server_capabilities = result
        .get("capabilities")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            McpSessionError::fatal(format!(
                "外部 MCP server `{server_name}` 的 initialize 结果缺少 capabilities。"
            ))
        })?;
    let server_info = result
        .get("serverInfo")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            McpSessionError::fatal(format!(
                "外部 MCP server `{server_name}` 的 initialize 结果缺少 serverInfo。"
            ))
        })?;
    Ok(McpNegotiatedSession {
        protocol_version: protocol_version.to_string(),
        server_capabilities,
        server_info,
        instructions: result
            .get("instructions")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

fn validate_session_id(server_name: &str, session_id: Option<&str>) -> Result<(), McpSessionError> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    if session_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Ok(());
    }
    Err(McpSessionError::fatal(format!(
        "外部 MCP server `{server_name}` 返回了包含不可见字符的 Mcp-Session-Id。"
    )))
}

fn build_http_headers(
    server_name: &str,
    configured: &BTreeMap<String, String>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    for (key, value) in configured {
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|error| {
            format!("外部 MCP server `{server_name}` 的 Header 名称无效：{error}")
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            format!("外部 MCP server `{server_name}` 的 Header `{key}` 值无效：{error}")
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn parse_http_response(
    text: &str,
    expected_id: u64,
) -> Result<ParsedHttpResponse, McpSessionError> {
    let mut payloads = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "[DONE]")
        .collect::<Vec<_>>();
    if payloads.is_empty() && !text.trim().is_empty() {
        payloads.push(text.trim());
    }
    let mut response = None;
    let mut side_messages = Vec::new();
    for payload in payloads {
        let value: Value = serde_json::from_str(payload).map_err(|error| {
            McpSessionError::fatal(format!("解析外部 MCP HTTP JSON-RPC 响应失败：{error}"))
        })?;
        if !value.is_object() {
            return Err(McpSessionError::fatal(
                "外部 MCP HTTP 返回了非对象 JSON-RPC 消息。",
            ));
        }
        if json_rpc_id(value.get("id")) == Some(expected_id) && value.get("method").is_none() {
            response = Some(extract_response_result(&value));
        } else {
            side_messages.push(value);
        }
    }
    Ok(ParsedHttpResponse {
        response,
        side_messages,
    })
}

fn next_sse_frame(buffer: &[u8]) -> Option<(String, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    let (end, separator) = match (lf, crlf) {
        (Some(lf), Some(crlf)) => std::cmp::min(lf, crlf),
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return None,
    };
    Some((
        String::from_utf8_lossy(&buffer[..end]).into_owned(),
        end + separator,
    ))
}

fn parse_sse_messages(frame: &str) -> Result<Vec<Value>, McpSessionError> {
    let data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.trim().is_empty() || data.trim() == "[DONE]" {
        return Ok(Vec::new());
    }
    let value = serde_json::from_str::<Value>(&data).map_err(|error| {
        McpSessionError::fatal(format!("解析外部 MCP HTTP SSE 消息失败：{error}"))
    })?;
    if !value.is_object() {
        return Err(McpSessionError::fatal(
            "外部 MCP HTTP SSE 返回了非对象 JSON-RPC 消息。",
        ));
    }
    Ok(vec![value])
}

fn extract_response_result(value: &Value) -> Result<Value, McpSessionError> {
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32_603);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知 JSON-RPC 错误");
        return Err(McpSessionError::server(format!(
            "JSON-RPC 错误 {code}：{message}"
        )));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| McpSessionError::fatal("JSON-RPC 响应缺少 result 或 error。"))
}

fn response_for_server_request(value: &Value) -> Option<Value> {
    let id = value.get("id")?.clone();
    let method = value.get("method")?.as_str()?;
    Some(if method == "ping" {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": "Muse 未声明并实现该 MCP client capability。"
            }
        })
    })
}

fn json_rpc_request(id: u64, method: &str, params: Option<Value>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params.unwrap_or_else(|| serde_json::json!({}))
    })
}

fn json_rpc_notification(method: &str, params: Option<Value>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params.unwrap_or_else(|| serde_json::json!({}))
    })
}

fn cancellation_notification(request_id: u64, reason: &str) -> Value {
    json_rpc_notification(
        "notifications/cancelled",
        Some(serde_json::json!({
            "requestId": request_id,
            "reason": reason
        })),
    )
}

fn json_rpc_id(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn fail_all_pending(pending: &PendingResponses, error: McpSessionError) {
    let responses = std::mem::take(&mut *lock_pending(pending));
    for (_, sender) in responses {
        let _ = sender.send(Err(error.clone()));
    }
}

fn lock_pending(
    pending: &PendingResponses,
) -> std::sync::MutexGuard<'_, BTreeMap<u64, PendingResponse>> {
    lock_std(pending)
}

fn lock_std<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn truncate_error_text(text: &str) -> String {
    const MAX: usize = 2_000;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    format!(
        "{}\n...（已截断）",
        text.chars().take(MAX).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHttpState {
        initialize_count: AtomicU64,
        tool_call_attempts: AtomicU64,
        cancellation_count: AtomicU64,
        delete_count: AtomicU64,
        get_count: AtomicU64,
        resource_list_attempts: AtomicU64,
        reject_next_tool_call: AtomicBool,
        fail_next_tool_call: AtomicBool,
        cursor_loop: AtomicBool,
        advertise_resources: AtomicBool,
        sse_notification_delay_ms: Option<u64>,
        session_headers: StdMutex<Vec<String>>,
        protocol_headers: StdMutex<Vec<String>>,
    }

    impl MockHttpState {
        fn new(reject_next_tool_call: bool, sse_notification_delay_ms: Option<u64>) -> Arc<Self> {
            Arc::new(Self {
                initialize_count: AtomicU64::new(0),
                tool_call_attempts: AtomicU64::new(0),
                cancellation_count: AtomicU64::new(0),
                delete_count: AtomicU64::new(0),
                get_count: AtomicU64::new(0),
                resource_list_attempts: AtomicU64::new(0),
                reject_next_tool_call: AtomicBool::new(reject_next_tool_call),
                fail_next_tool_call: AtomicBool::new(false),
                cursor_loop: AtomicBool::new(false),
                advertise_resources: AtomicBool::new(true),
                sse_notification_delay_ms,
                session_headers: StdMutex::new(Vec::new()),
                protocol_headers: StdMutex::new(Vec::new()),
            })
        }
    }

    async fn spawn_http_mcp_server(
        reject_next_tool_call: bool,
        sse_notification_delay_ms: Option<u64>,
    ) -> (String, Arc<MockHttpState>, oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应能绑定测试 HTTP MCP 端口");
        let address = listener.local_addr().expect("应能读取测试端口");
        let state = MockHttpState::new(reject_next_tool_call, sse_notification_delay_ms);
        let state_for_task = Arc::clone(&state);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        let state = Arc::clone(&state_for_task);
                        tokio::spawn(async move {
                            handle_http_mcp_connection(stream, state).await;
                        });
                    }
                }
            }
        });
        (format!("http://{address}/mcp"), state, shutdown_tx)
    }

    async fn handle_http_mcp_connection(
        mut stream: tokio::net::TcpStream,
        state: Arc<MockHttpState>,
    ) {
        let Some((method, headers, body)) = read_http_request(&mut stream).await else {
            return;
        };
        if let Some(session_id) = headers.get("mcp-session-id") {
            lock_std(&state.session_headers).push(session_id.clone());
        }
        if let Some(protocol_version) = headers.get("mcp-protocol-version") {
            lock_std(&state.protocol_headers).push(protocol_version.clone());
        }
        if method == "GET" {
            state.get_count.fetch_add(1, Ordering::Relaxed);
            if let Some(delay_ms) = state.sse_notification_delay_ms {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                write_http_response(
                    &mut stream,
                    "200 OK",
                    "text/event-stream",
                    "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n",
                )
                .await;
            } else {
                write_http_response(
                    &mut stream,
                    "405 Method Not Allowed",
                    "text/plain",
                    "不支持独立 SSE 接收器",
                )
                .await;
            }
            return;
        }
        if method == "DELETE" {
            state.delete_count.fetch_add(1, Ordering::Relaxed);
            write_http_response(&mut stream, "200 OK", "application/json", "").await;
            return;
        }
        let payload: Value =
            serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
        let rpc_method = payload
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = payload.get("id").cloned().unwrap_or(Value::Null);
        match rpc_method {
            "initialize" => {
                let count = state.initialize_count.fetch_add(1, Ordering::Relaxed) + 1;
                let capabilities = if state.advertise_resources.load(Ordering::Acquire) {
                    serde_json::json!({
                        "tools": { "listChanged": true },
                        "resources": { "listChanged": true }
                    })
                } else {
                    serde_json::json!({ "tools": { "listChanged": true } })
                };
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": capabilities,
                        "serverInfo": { "name": "mock-http", "version": "1" },
                        "instructions": "测试连接"
                    }
                })
                .to_string();
                write_http_response_with_headers(
                    &mut stream,
                    "200 OK",
                    "application/json",
                    &body,
                    &[("Mcp-Session-Id", format!("session-{count}"))],
                )
                .await;
            }
            "notifications/initialized" => {
                write_http_response(&mut stream, "202 Accepted", "application/json", "").await;
            }
            "notifications/cancelled" => {
                state.cancellation_count.fetch_add(1, Ordering::Relaxed);
                write_http_response(&mut stream, "202 Accepted", "application/json", "").await;
            }
            "tools/list" => {
                let cursor = payload.pointer("/params/cursor").and_then(Value::as_str);
                let result = if state.cursor_loop.load(Ordering::Acquire) {
                    serde_json::json!({
                        "tools": [],
                        "nextCursor": "loop"
                    })
                } else if cursor == Some("page-2") {
                    serde_json::json!({
                        "tools": [{
                            "name": "second",
                            "description": "第二页工具",
                            "inputSchema": { "type": "object" }
                        }]
                    })
                } else {
                    serde_json::json!({
                        "tools": [{
                            "name": "first",
                            "description": "第一页工具",
                            "inputSchema": { "type": "object" }
                        }],
                        "nextCursor": "page-2"
                    })
                };
                write_json_rpc_result(&mut stream, id, result).await;
            }
            "tools/call" => {
                state.tool_call_attempts.fetch_add(1, Ordering::Relaxed);
                if payload.pointer("/params/name").and_then(Value::as_str) == Some("slow") {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                if state.fail_next_tool_call.swap(false, Ordering::AcqRel) {
                    write_http_response(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "server crashed",
                    )
                    .await;
                    return;
                }
                if state.reject_next_tool_call.swap(false, Ordering::AcqRel) {
                    write_http_response(
                        &mut stream,
                        "404 Not Found",
                        "text/plain",
                        "session expired",
                    )
                    .await;
                    return;
                }
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": "ok" }],
                        "isError": false,
                        "meta": {
                            "session": headers.get("mcp-session-id").cloned()
                        }
                    }
                });
                let body = format!(
                    "event: message\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}}\n\nevent: message\ndata: {}\n\n",
                    response
                );
                write_http_response(&mut stream, "200 OK", "text/event-stream", &body).await;
            }
            "resources/list" => {
                state.resource_list_attempts.fetch_add(1, Ordering::Relaxed);
                write_json_rpc_result(
                    &mut stream,
                    id,
                    serde_json::json!({
                        "resources": [{ "uri": "muse://resource", "name": "资源" }],
                        "nextCursor": "resource-next"
                    }),
                )
                .await;
            }
            "resources/templates/list" => {
                write_json_rpc_result(
                    &mut stream,
                    id,
                    serde_json::json!({
                        "resourceTemplates": [{
                            "uriTemplate": "muse://resource/{id}",
                            "name": "资源模板"
                        }],
                        "nextCursor": "template-next"
                    }),
                )
                .await;
            }
            _ => {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "未知方法" }
                })
                .to_string();
                write_http_response(&mut stream, "200 OK", "application/json", &body).await;
            }
        }
    }

    async fn read_http_request(
        stream: &mut tokio::net::TcpStream,
    ) -> Option<(String, BTreeMap<String, String>, Vec<u8>)> {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4_096];
        let header_end = loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
            if buffer.len() > 64 * 1024 {
                return None;
            }
        };
        let headers_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = headers_text.split("\r\n");
        let method = lines.next()?.split_whitespace().next()?.to_string();
        let mut headers = BTreeMap::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();
        while buffer.len() < header_end + content_length {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        Some((
            method,
            headers,
            buffer[header_end..header_end + content_length].to_vec(),
        ))
    }

    async fn write_json_rpc_result(stream: &mut tokio::net::TcpStream, id: Value, result: Value) {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
        .to_string();
        write_http_response(stream, "200 OK", "application/json", &body).await;
    }

    async fn write_http_response(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        content_type: &str,
        body: &str,
    ) {
        write_http_response_with_headers(stream, status, content_type, body, &[]).await;
    }

    async fn write_http_response_with_headers(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        content_type: &str,
        body: &str,
        headers: &[(&str, String)],
    ) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    fn http_server_config(url: String, revision: &str) -> ExternalMcpServerConfig {
        ExternalMcpServerConfig {
            name: "mock-http".to_string(),
            config_revision: revision.to_string(),
            enabled: true,
            request_timeout_ms: Some(2_000),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            transport: ExternalMcpTransport::StreamableHttp {
                url,
                headers: BTreeMap::new(),
            },
        }
    }

    async fn wait_for_counter(counter: &AtomicU64, expected: u64) {
        for _ in 0..100 {
            if counter.load(Ordering::Acquire) >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等待测试计数达到 {expected} 超时");
    }

    #[test]
    fn initialize_declares_only_real_client_capabilities() {
        let params = initialize_params();
        assert_eq!(params["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(params["capabilities"], serde_json::json!({}));
        assert!(params["capabilities"].get("tools").is_none());
        assert!(params["capabilities"].get("resources").is_none());
    }

    #[test]
    fn negotiation_accepts_stable_and_compatible_versions_only() {
        for version in [MCP_PROTOCOL_VERSION, MCP_COMPATIBLE_PROTOCOL_VERSION] {
            let result = validate_initialize_result(
                "demo",
                serde_json::json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "demo", "version": "1" }
                }),
            )
            .expect("受支持版本应通过协商");
            assert_eq!(result.protocol_version, version);
        }
        let error = validate_initialize_result(
            "demo",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": { "name": "demo", "version": "1" }
            }),
        )
        .expect_err("不支持版本必须拒绝");
        assert!(error.message.contains("不兼容协议版本"));
    }

    #[test]
    fn parses_sse_response_and_observes_side_notification() {
        let parsed = parse_http_response(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"tools\":[]}}\n\n",
            7,
        )
        .expect("SSE 应可解析");
        assert_eq!(parsed.side_messages.len(), 1);
        assert_eq!(
            parsed.response.expect("应有响应").expect("响应应成功"),
            serde_json::json!({ "tools": [] })
        );
    }

    #[tokio::test]
    async fn http_session_reuses_initialization_paginates_and_invalidates_future_catalog() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, Some(300)).await;
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        let negotiated = lease.session.negotiated.read().await.clone();
        assert_eq!(negotiated.protocol_version, MCP_PROTOCOL_VERSION);
        assert!(negotiated.server_capabilities.get("tools").is_some());

        let tools = super::super::list_all_external_mcp_tools(&lease)
            .await
            .expect("工具目录应有界拉全");
        assert_eq!(tools["tools"].as_array().map(Vec::len), Some(2));
        assert_eq!(state.initialize_count.load(Ordering::Acquire), 1);

        let epoch = lease.catalog_epoch();
        for _ in 0..100 {
            if lease.catalog_epoch() > epoch {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            lease.catalog_epoch() > epoch,
            "GET SSE 中的 listChanged 应使后续目录失效"
        );
        assert_eq!(state.get_count.load(Ordering::Acquire), 1);
        let epoch = lease.catalog_epoch();
        let result = lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "first", "arguments": {} })),
            )
            .await
            .expect("同一 HTTP session 应可调用工具");
        assert_eq!(result["content"][0]["text"], "ok");
        assert!(
            lease.catalog_epoch() > epoch,
            "listChanged 应使后续目录失效"
        );
        assert_eq!(state.initialize_count.load(Ordering::Acquire), 1);
        assert!(
            lock_std(&state.session_headers)
                .iter()
                .all(|session| session == "session-1")
        );
        assert!(
            lock_std(&state.protocol_headers)
                .iter()
                .all(|version| version == MCP_PROTOCOL_VERSION)
        );

        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn http_404_reinitializes_once_and_uses_new_session_id() {
        let (url, state, shutdown) = spawn_http_mcp_server(true, None).await;
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "first", "arguments": {} })),
            )
            .await
            .expect("404 后应重建 session 并重试一次");
        assert_eq!(state.initialize_count.load(Ordering::Acquire), 2);
        assert_eq!(state.tool_call_attempts.load(Ordering::Acquire), 2);
        let sessions = lock_std(&state.session_headers).clone();
        assert!(sessions.iter().any(|session| session == "session-1"));
        assert!(sessions.iter().any(|session| session == "session-2"));
        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn fatal_http_failure_evicts_session_and_next_lease_reinitializes() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        let manager = McpClientManager::default();
        let config = http_server_config(url, "revision-a");
        let failed_lease = manager.lease(&config).await.expect("HTTP MCP 应初始化");
        state.fail_next_tool_call.store(true, Ordering::Release);
        let error = failed_lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "first", "arguments": {} })),
            )
            .await
            .expect_err("服务端致命错误必须淘汰 session");
        assert!(error.contains("500 Internal Server Error"));
        assert!(!failed_lease.is_healthy());
        wait_for_counter(&state.delete_count, 1).await;

        let recovered_lease = manager
            .lease(&config)
            .await
            .expect("下一次 lease 应重新初始化");
        assert!(!Arc::ptr_eq(
            &failed_lease.session,
            &recovered_lease.session
        ));
        assert_eq!(state.initialize_count.load(Ordering::Acquire), 2);
        drop(failed_lease);
        drop(recovered_lease);
        wait_for_counter(&state.delete_count, 2).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn config_revision_keeps_old_http_lease_stable_until_last_holder_drops() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        let manager = McpClientManager::default();
        let old_lease = manager
            .lease(&http_server_config(url.clone(), "revision-a"))
            .await
            .expect("旧 revision 应初始化");
        let new_lease = manager
            .lease(&http_server_config(url, "revision-b"))
            .await
            .expect("新 revision 应建立独立 session");
        assert!(!Arc::ptr_eq(&old_lease.session, &new_lease.session));
        assert_eq!(state.initialize_count.load(Ordering::Acquire), 2);

        let old_result = old_lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "first", "arguments": {} })),
            )
            .await
            .expect("旧 lease 应继续使用旧 session");
        let new_result = new_lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "first", "arguments": {} })),
            )
            .await
            .expect("新 lease 应使用新 session");
        assert_eq!(old_result["meta"]["session"], "session-1");
        assert_eq!(new_result["meta"]["session"], "session-2");

        drop(old_lease);
        wait_for_counter(&state.delete_count, 1).await;
        assert!(new_lease.is_healthy());
        drop(new_lease);
        wait_for_counter(&state.delete_count, 2).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn tools_list_rejects_cursor_loop() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        state.cursor_loop.store(true, Ordering::Release);
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        let error = super::super::list_all_external_mcp_tools(&lease)
            .await
            .expect_err("cursor 循环必须失败");
        assert!(error.contains("cursor 循环"));
        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn missing_server_capability_blocks_operation_before_transport() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        state.advertise_resources.store(false, Ordering::Release);
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        let error = lease
            .request("resources/list", Some(serde_json::json!({})))
            .await
            .expect_err("未协商 resources capability 时必须拒绝调用");
        assert!(error.contains("未声明 `resources` capability"));
        assert_eq!(state.resource_list_attempts.load(Ordering::Acquire), 0);
        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn resource_and_template_lists_preserve_server_cursors() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        let leases = vec![lease.clone()];
        let resources = super::super::list_external_mcp_resources_with_leases(
            &leases,
            std::env::temp_dir().join("muse-mcp-test.toml"),
            Some("mock-http".to_string()),
            None,
        )
        .await
        .expect("资源列表应保留 cursor");
        assert_eq!(resources.resources.len(), 1);
        assert_eq!(resources.next_cursor.as_deref(), Some("resource-next"));
        let templates = super::super::list_external_mcp_resource_templates_with_leases(
            &leases,
            std::env::temp_dir().join("muse-mcp-test.toml"),
            Some("mock-http".to_string()),
            None,
        )
        .await
        .expect("资源模板列表应保留 cursor");
        assert_eq!(templates.resource_templates.len(), 1);
        assert_eq!(templates.next_cursor.as_deref(), Some("template-next"));

        drop(leases);
        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn dropping_http_request_sends_cancellation_notification() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        let manager = McpClientManager::default();
        let lease = manager
            .lease(&http_server_config(url, "revision-a"))
            .await
            .expect("HTTP MCP 应初始化");
        let pending_lease = lease.clone();
        let request = tokio::spawn(async move {
            pending_lease
                .request(
                    "tools/call",
                    Some(serde_json::json!({ "name": "slow", "arguments": {} })),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        request.abort();
        let _ = request.await;
        wait_for_counter(&state.cancellation_count, 1).await;

        drop(lease);
        wait_for_counter(&state.delete_count, 1).await;
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn http_timeout_sends_cancellation_before_evicting_session() {
        let (url, state, shutdown) = spawn_http_mcp_server(false, None).await;
        let manager = McpClientManager::default();
        let mut config = http_server_config(url, "revision-a");
        config.request_timeout_ms = Some(100);
        let lease = manager.lease(&config).await.expect("HTTP MCP 应初始化");
        let error = lease
            .request(
                "tools/call",
                Some(serde_json::json!({ "name": "slow", "arguments": {} })),
            )
            .await
            .expect_err("超时请求必须失败");
        assert!(error.contains("超时"));
        wait_for_counter(&state.cancellation_count, 1).await;
        wait_for_counter(&state.delete_count, 1).await;
        assert!(!lease.is_healthy());

        drop(lease);
        let _ = shutdown.send(());
    }

    #[test]
    fn stderr_diagnostic_is_bounded_and_redacts_configured_secrets() {
        let diagnostic = BoundedDiagnostic::new(vec!["secret-token-xxxxxxxx".to_string()]);
        diagnostic.push(&format!(
            "secret-token-xxxxxxxx {}",
            "日志".repeat(STDERR_DIAGNOSTIC_BYTES)
        ));
        let summary = diagnostic.summary();
        assert!(!summary.contains("secret-token-xxxxxxxx"));
        assert!(summary.len() <= STDERR_DIAGNOSTIC_BYTES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_session_reuses_process_drains_stderr_cancels_and_reaps_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "muse-mcp-session-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("应能创建 MCP 测试目录");
        let script = root.join("server.sh");
        let cancelled = root.join("cancelled");
        let child_pid_file = root.join("child.pid");
        let initialize_count_file = root.join("initialize.count");
        let script_content = r#"#!/bin/sh
cancelled=$1
child_pid_file=$2
initialize_count_file=$3
initialize_count=0
call_count=0
sleep 30 &
echo $! > "$child_pid_file"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      initialize_count=$((initialize_count + 1))
      printf '%s' "$initialize_count" > "$initialize_count_file"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"mock-stdio","version":"1"}}}\n' "$id"
      ;;
    *'"method":"notifications/cancelled"'*)
      printf 'cancelled' > "$cancelled"
      ;;
    *'"method":"tools/list"'*)
      call_count=$((call_count + 1))
      if [ "$call_count" -eq 1 ]; then
        i=0
        while [ "$i" -lt 5000 ]; do
          printf 'secret-token-xxxxxxxx stderr flood\n' >&2
          i=$((i + 1))
        done
      fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"state","description":"count-%s","inputSchema":{"type":"object"}}]}}\n' "$id" "$call_count"
      ;;
    *'"method":"tools/call"'*'"name":"slow"'*)
      ;;
    *'"method":"tools/call"'*'"name":"delayed"'*)
      (sleep 0.2; printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"delayed"}]}}\n' "$id") &
      ;;
    *'"method":"tools/call"'*'"name":"fast"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"fast"}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}]}}\n' "$id"
      ;;
  esac
done
"#;
        std::fs::write(&script, script_content).expect("应能写入 MCP 测试脚本");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("应能设置 MCP 测试脚本权限");

        let config = ExternalMcpServerConfig {
            name: "mock-stdio".to_string(),
            config_revision: "revision-a".to_string(),
            enabled: true,
            request_timeout_ms: Some(5_000),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            transport: ExternalMcpTransport::Stdio {
                command: script.display().to_string(),
                args: vec![
                    cancelled.display().to_string(),
                    child_pid_file.display().to_string(),
                    initialize_count_file.display().to_string(),
                ],
                env: BTreeMap::from([("TOKEN".to_string(), "secret-token-xxxxxxxx".to_string())]),
                cwd: Some(root.display().to_string()),
            },
        };
        let manager = McpClientManager::default();
        let lease = manager.lease(&config).await.expect("stdio MCP 应初始化");
        let second_lease = manager
            .lease(&config)
            .await
            .expect("同 revision 应复用 stdio MCP");
        assert!(Arc::ptr_eq(&lease.session, &second_lease.session));

        let first = lease
            .request("tools/list", Some(serde_json::json!({})))
            .await
            .expect("stderr 洪泛不能阻塞 tools/list");
        let second = second_lease
            .request("tools/list", Some(serde_json::json!({})))
            .await
            .expect("同一进程状态应连续");
        assert_eq!(first["tools"][0]["description"], "count-1");
        assert_eq!(second["tools"][0]["description"], "count-2");
        assert_eq!(
            std::fs::read_to_string(&initialize_count_file).expect("应记录初始化次数"),
            "1"
        );

        let delayed_lease = lease.clone();
        let delayed = tokio::spawn(async move {
            delayed_lease
                .request(
                    "tools/call",
                    Some(serde_json::json!({ "name": "delayed", "arguments": {} })),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let fast_lease = lease.clone();
        let fast = tokio::spawn(async move {
            fast_lease
                .request(
                    "tools/call",
                    Some(serde_json::json!({ "name": "fast", "arguments": {} })),
                )
                .await
        });
        assert_eq!(
            fast.await
                .expect("快速请求任务不应崩溃")
                .expect("快速响应应按 id 路由")["content"][0]["text"],
            "fast"
        );
        assert_eq!(
            delayed
                .await
                .expect("延迟请求任务不应崩溃")
                .expect("乱序响应应回到原请求")["content"][0]["text"],
            "delayed"
        );

        let slow_lease = second_lease.clone();
        let slow = tokio::spawn(async move {
            slow_lease
                .request(
                    "tools/call",
                    Some(serde_json::json!({ "name": "slow", "arguments": {} })),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        slow.abort();
        let _ = slow.await;
        for _ in 0..100 {
            if cancelled.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            cancelled.exists(),
            "取消 Future 必须发送 notifications/cancelled"
        );

        let child_pid = std::fs::read_to_string(&child_pid_file)
            .expect("应记录后代进程 PID")
            .trim()
            .parse::<libc::pid_t>()
            .expect("后代进程 PID 应合法");
        manager.request_close_all();
        for _ in 0..150 {
            if unsafe { libc::kill(child_pid, 0) } != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_ne!(
            unsafe { libc::kill(child_pid, 0) },
            0,
            "运行时关闭 manager 后不得遗留 stdio 后代进程"
        );
        assert!(!lease.is_healthy());
        drop(lease);
        drop(second_lease);
        let _ = std::fs::remove_dir_all(root);
    }
}
