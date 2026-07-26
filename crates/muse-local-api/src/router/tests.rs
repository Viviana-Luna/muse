use super::*;
use crate::dto::{MemoryCorrectRequest, MemoryCreateRequest, MemoryImportanceAdjustRequest};
use crate::state::{AppState, MemoryRuntimeServices};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use futures::StreamExt;
use muse_core::app::memory_safety::DeterministicMemorySensitivityPolicy;
use muse_core::domain::conversation::{Conversation, Role};
use muse_core::domain::memory::{
    ConfirmedMemoryDeleteRequest, MemoryBatchCommitReceipt, MemoryCommitEnvelope,
    MemoryDeleteReceipt, MemoryDeletionAuthority, MemoryDeletionAuthorityReceipt,
    MemoryDeletionAuthorityRequest, MemoryDeletionCheckRequest, MemoryDeletionDecision,
    MemoryError, MemoryErrorCode, MemoryFacet, MemoryId, MemoryImportanceAdjustment,
    MemoryImportanceAdjustmentReceipt, MemoryManagementContentMutation, MemoryMutationReceipt,
    MemoryMutationReceiptState, MemoryPersonaScope, MemoryQueryPageReceipt, MemoryRecord,
    MemoryRepository, MemoryRetrievalRequest, MemoryRetriever, MemorySensitivityPolicy,
};
use muse_core::domain::persona::character::store::PersonaStore;
use muse_core::domain::persona::visual::store::VisualPackStore;
use muse_core::domain::persona::{Persona, RoleplayStyle, ToolPolicy, VisualPack};
use muse_core::domain::tool::{ToolCall, ToolCallSource, ToolDef, ToolRegistry, builtin};
use muse_core::model::provider::{
    ChatModelError, ChatModelProvider, ChatModelResult, ChatStreamEvent, ChatStreamResult,
};
use muse_runtime::interactions::{PendingApproval, PendingUserQuestion};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{Mutex, Semaphore, broadcast, oneshot};
use tower::util::ServiceExt;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let suffix = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("muse-router-{prefix}-{nanos}-{suffix}"));
    std::fs::create_dir_all(&dir).expect("应能创建测试临时目录");
    dir
}

fn snapshot_regular_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut files = std::fs::read_dir(dir)
        .expect("应能读取测试文件目录")
        .map(|entry| {
            let entry = entry.expect("测试文件目录项应可读取");
            let file_type = entry.file_type().expect("应能读取测试文件类型");
            assert!(file_type.is_file(), "测试快照目录只能包含普通文件");
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).expect("应能读取测试快照文件"),
            )
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

struct ScriptedChatProvider {
    calls: AtomicUsize,
}

struct CountingChatProvider {
    calls: AtomicUsize,
}

struct GatedChatProvider {
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl ChatModelProvider for GatedChatProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        Ok("门控测试回复。".to_string())
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolDef],
    ) -> ChatStreamResult {
        let entered = self.entered.clone();
        let release = self.release.clone();
        let gated_reply = futures::stream::once(async move {
            entered.add_permits(1);
            let permit = release.acquire().await.expect("测试释放闸门不应关闭");
            permit.forget();
            Ok(ChatStreamEvent::Text("门控测试回复。".to_string()))
        });
        Box::pin(gated_reply.chain(futures::stream::iter([Ok(ChatStreamEvent::Done)])))
    }

    fn name(&self) -> &'static str {
        "gated_test"
    }
}

struct FailingChatProvider;

#[derive(Clone)]
enum MemoryProviderFollowup {
    Success,
    StreamError,
    Pending(Arc<Semaphore>),
}

struct MemoryLifecycleProvider {
    calls: AtomicUsize,
    followup: MemoryProviderFollowup,
}

#[derive(Default)]
struct MemoryContractProviderState {
    current_memory_id: Option<String>,
    current_revision_id: Option<String>,
    query_results_for_model: Vec<String>,
}

struct MemoryContractProvider {
    state: StdMutex<MemoryContractProviderState>,
}

impl MemoryContractProvider {
    fn new() -> Self {
        Self {
            state: StdMutex::new(MemoryContractProviderState::default()),
        }
    }

    fn set_current(&self, memory_id: &MemoryId, revision_id: &MemoryRevisionId) {
        let mut state = self.state.lock().expect("记忆合同 Provider 状态锁不应中毒");
        state.current_memory_id = Some(memory_id.0.clone());
        state.current_revision_id = Some(revision_id.0.clone());
    }

    fn current_identifiers(&self) -> (String, String) {
        let state = self.state.lock().expect("记忆合同 Provider 状态锁不应中毒");
        (
            state
                .current_memory_id
                .clone()
                .expect("update/correct 前应注入当前 memory_id"),
            state
                .current_revision_id
                .clone()
                .expect("update/correct 前应注入当前 revision_id"),
        )
    }

    fn query_results_for_model(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("记忆合同 Provider 状态锁不应中毒")
            .query_results_for_model
            .clone()
    }
}

#[async_trait::async_trait]
impl ChatModelProvider for MemoryContractProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        Ok("记忆合同测试回复。".to_string())
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        conversation: &Conversation,
        tools: &[ToolDef],
    ) -> ChatStreamResult {
        assert!(
            tools.iter().any(|tool| tool.name == "memory_query")
                && tools.iter().any(|tool| tool.name == "memory_mutate"),
            "正常 Persona 回合必须冻结两个自动记忆 Tool"
        );

        if let Some(last) = conversation.messages.last()
            && last.role == Role::Tool
        {
            if last.tool_name.as_deref() == Some("memory_query") {
                self.state
                    .lock()
                    .expect("记忆合同 Provider 状态锁不应中毒")
                    .query_results_for_model
                    .push(last.content.clone());
            }
            return Box::pin(futures::stream::iter([
                Ok(ChatStreamEvent::Text("记忆合同步骤已完成。".to_string())),
                Ok(ChatStreamEvent::Done),
            ]));
        }

        let direct_user_message = conversation
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| message.content.as_str())
            .expect("记忆合同初始模型调用应包含直接用户消息");
        let (call_id, name, arguments) = match direct_user_message {
            "我喜欢在晚上喝茉莉花茶" => (
                "memory-contract-create",
                "memory_mutate",
                serde_json::json!({
                    "mutations": [{
                        "operation": "create",
                        "source_quote": "我喜欢在晚上喝茉莉花茶",
                        "category": "user_preference",
                        "facet": "preference_drink",
                        "keywords": ["茉莉花茶", "晚上", "饮品偏好"],
                        "content": "用户偏好在夜间饮用茉莉花茶。",
                        "importance": "normal",
                        "event_time": null,
                        "change_reason": "用户在当前消息中直接说明饮品偏好",
                        "confirmation": "not_required"
                    }]
                }),
            ),
            "请回忆我的茉莉花茶偏好" | "重启后请再次回忆我的茉莉花茶偏好" => {
                (
                    if direct_user_message.starts_with("重启后") {
                        "memory-contract-query-after-restart"
                    } else {
                        "memory-contract-query-cross-session"
                    },
                    "memory_query",
                    serde_json::json!({
                        "query": "茉莉花茶",
                        "limit": 5,
                        "cursor": null,
                        "as_of": null,
                        "memory_id": null,
                        "include_history": false
                    }),
                )
            }
            "我喜欢在晚上喝红茶" => {
                let (memory_id, revision_id) = self.current_identifiers();
                (
                    "memory-contract-update",
                    "memory_mutate",
                    serde_json::json!({
                        "mutations": [{
                            "operation": "update",
                            "source_quote": "我喜欢在晚上喝红茶",
                            "memory_id": memory_id,
                            "expected_revision_id": revision_id,
                            "category": "user_preference",
                            "facet": "preference_drink",
                            "keywords": ["红茶", "晚上", "饮品偏好"],
                            "content": "用户现在偏好在夜间饮用红茶。",
                            "importance": "normal",
                            "event_time": null,
                            "change_reason": "用户说明饮品偏好后来发生变化",
                            "confirmation": "not_required"
                        }]
                    }),
                )
            }
            "我喜欢在晚上喝无糖茉莉花茶" => {
                let (memory_id, revision_id) = self.current_identifiers();
                (
                    "memory-contract-correct",
                    "memory_mutate",
                    serde_json::json!({
                        "mutations": [{
                            "operation": "correct",
                            "source_quote": "我喜欢在晚上喝无糖茉莉花茶",
                            "memory_id": memory_id,
                            "expected_revision_id": revision_id,
                            "category": "user_preference",
                            "facet": "preference_drink",
                            "keywords": ["无糖茉莉花茶", "晚上", "饮品偏好"],
                            "content": "用户准确的偏好是在夜间饮用无糖茉莉花茶。",
                            "importance": "normal",
                            "event_time": null,
                            "change_reason": "用户纠正上一条饮品偏好",
                            "confirmation": "not_required"
                        }]
                    }),
                )
            }
            other => panic!("未定义的记忆合同测试消息：{other}"),
        };
        Box::pin(futures::stream::iter([
            Ok(ChatStreamEvent::ToolCall(ToolCall {
                call_id: call_id.to_string(),
                name: name.to_string(),
                arguments,
                source: ToolCallSource::Native,
            })),
            Ok(ChatStreamEvent::Done),
        ]))
    }

    fn name(&self) -> &'static str {
        "memory_contract_test"
    }
}

#[async_trait::async_trait]
impl ChatModelProvider for MemoryLifecycleProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        Ok("记忆生命周期测试回复。".to_string())
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolDef],
    ) -> ChatStreamResult {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Box::pin(futures::stream::iter([
                Ok(ChatStreamEvent::ToolCall(ToolCall {
                    call_id: "call-memory-lifecycle".to_string(),
                    name: "memory_mutate".to_string(),
                    arguments: serde_json::json!({
                        "operation": "create",
                        "category": "user_preference",
                        "content": "用户喜欢夜间散步",
                        "importance": "normal",
                        "event_time": null,
                        "change_reason": "用户在当前消息中直接说明"
                    }),
                    source: ToolCallSource::Native,
                })),
                Ok(ChatStreamEvent::Done),
            ]));
        }

        match &self.followup {
            MemoryProviderFollowup::Success => Box::pin(futures::stream::iter([
                Ok(ChatStreamEvent::Text("已经记下。".to_string())),
                Ok(ChatStreamEvent::Done),
            ])),
            MemoryProviderFollowup::StreamError => Box::pin(futures::stream::iter([Err(
                ChatModelError::ApiError("注入续写断流".to_string()),
            )])),
            MemoryProviderFollowup::Pending(entered) => {
                let entered = Arc::clone(entered);
                Box::pin(futures::stream::once(async move {
                    entered.add_permits(1);
                    std::future::pending::<Result<ChatStreamEvent, ChatModelError>>().await
                }))
            }
        }
    }

    fn name(&self) -> &'static str {
        "memory_lifecycle_test"
    }
}

struct EmptyMemoryRetriever;

impl MemoryRetriever for EmptyMemoryRetriever {
    fn retrieve(
        &self,
        _request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        Ok(MemoryQueryPageReceipt::new(Vec::new(), None))
    }
}

struct TestMemoryDeletionAuthority;

impl MemoryDeletionAuthority for TestMemoryDeletionAuthority {
    fn record(
        &self,
        _request: &MemoryDeletionAuthorityRequest,
    ) -> Result<MemoryDeletionAuthorityReceipt, MemoryError> {
        Err(MemoryError::new(
            MemoryErrorCode::DeletionAuthorityUnavailable,
        ))
    }

    fn check(
        &self,
        _request: &MemoryDeletionCheckRequest,
    ) -> Result<MemoryDeletionDecision, MemoryError> {
        Ok(MemoryDeletionDecision::Allowed)
    }
}

struct RecordingMemoryRepository {
    commit_calls: Arc<AtomicUsize>,
}

impl MemoryRepository for RecordingMemoryRepository {
    fn current(
        &self,
        _scope: &MemoryPersonaScope,
        _memory_id: &MemoryId,
    ) -> Result<Option<MemoryRecord>, MemoryError> {
        Ok(None)
    }

    fn apply_committed_batch(
        &self,
        envelope: &MemoryCommitEnvelope,
        _sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryBatchCommitReceipt, MemoryError> {
        self.commit_calls.fetch_add(1, Ordering::SeqCst);
        Ok(MemoryBatchCommitReceipt {
            idempotency_key: envelope.idempotency_key().to_string(),
            mutations: envelope
                .mutations()
                .iter()
                .map(|mutation| {
                    let mut receipt = mutation.staged_receipt();
                    receipt.state = MemoryMutationReceiptState::Durable;
                    receipt
                })
                .collect(),
            durable_at: envelope.committed_at().to_string(),
        })
    }

    fn apply_management_content_mutation(
        &self,
        _mutation: &MemoryManagementContentMutation,
        _sensitivity: &dyn MemorySensitivityPolicy,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::InvalidRequest))
    }

    fn adjust_importance(
        &self,
        _adjustment: &MemoryImportanceAdjustment,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::InvalidRequest))
    }

    fn delete_confirmed(
        &self,
        _request: &ConfirmedMemoryDeleteRequest,
        _authority: &dyn MemoryDeletionAuthority,
    ) -> Result<MemoryDeleteReceipt, MemoryError> {
        Err(MemoryError::new(MemoryErrorCode::InvalidRequest))
    }
}

#[async_trait::async_trait]
impl ChatModelProvider for CountingChatProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("计数测试回复。".to_string())
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolDef],
    ) -> ChatStreamResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([
            Ok(ChatStreamEvent::Text("计数测试回复。".to_string())),
            Ok(ChatStreamEvent::Done),
        ]))
    }

    fn name(&self) -> &'static str {
        "counting_test"
    }
}

#[async_trait::async_trait]
impl ChatModelProvider for FailingChatProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        Err(ChatModelError::ApiError("注入模型失败".to_string()))
    }

    async fn chat_stream(&self, _conversation: &Conversation) -> ChatStreamResult {
        Box::pin(futures::stream::iter([Err(ChatModelError::ApiError(
            "注入模型失败".to_string(),
        ))]))
    }

    fn name(&self) -> &'static str {
        "failing_test"
    }
}

#[async_trait::async_trait]
impl ChatModelProvider for ScriptedChatProvider {
    async fn chat(&self, _conversation: &Conversation) -> ChatModelResult {
        Ok("测试回复。".to_string())
    }

    async fn chat_stream(&self, conversation: &Conversation) -> ChatStreamResult {
        self.chat_stream_with_tools(conversation, &[]).await
    }

    async fn chat_stream_with_tools(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolDef],
    ) -> ChatStreamResult {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if call_index == 0 {
            vec![
                Ok(ChatStreamEvent::ToolCall(ToolCall {
                    call_id: "call-test-model-info".to_string(),
                    name: "model_info".to_string(),
                    arguments: serde_json::json!({}),
                    source: ToolCallSource::Native,
                })),
                Ok(ChatStreamEvent::Done),
            ]
        } else {
            vec![
                    Ok(ChatStreamEvent::Text(
                        "{\"emotion\":\"happy\",\"intensity\":70,\"reason_code\":\"positive_interaction\"}\n当前测试回复。"
                            .to_string(),
                    )),
                    Ok(ChatStreamEvent::Done),
                ]
        };
        Box::pin(futures::stream::iter(events))
    }

    fn name(&self) -> &'static str {
        "scripted_test"
    }
}

fn build_test_state(config_dir: &Path) -> Arc<AppState> {
    build_test_state_with_persona(config_dir, Some(test_persona("router-test-persona")))
}

fn build_empty_test_state(config_dir: &Path) -> Arc<AppState> {
    build_test_state_with_persona(config_dir, None)
}

fn build_memory_test_state(config_dir: &Path, commit_calls: Arc<AtomicUsize>) -> Arc<AppState> {
    let mut state = build_test_state(config_dir);
    Arc::get_mut(&mut state).expect("测试状态尚未共享").memory = Some(MemoryRuntimeServices {
        retriever: Arc::new(EmptyMemoryRetriever),
        repository: Arc::new(RecordingMemoryRepository { commit_calls }),
        sensitivity: Arc::new(DeterministicMemorySensitivityPolicy::new()),
        deletion_authority: Arc::new(TestMemoryDeletionAuthority),
        query_call_budget: NonZeroU32::new(2).expect("测试查询预算必须非零"),
    });
    state
}

fn memory_lifecycle_request(client_request_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/chat/stream")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "message": "我喜欢夜间散步",
                "conversation_id": "default",
                "client_request_id": client_request_id
            })
            .to_string(),
        ))
        .expect("应能构造记忆生命周期请求")
}

fn memory_contract_request(
    conversation_id: &str,
    client_request_id: &str,
    message: &str,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/chat/stream")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "message": message,
                "conversation_id": conversation_id,
                "client_request_id": client_request_id
            })
            .to_string(),
        ))
        .expect("应能构造自动记忆合同请求")
}

async fn read_sse_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("应能读取记忆生命周期 SSE")
            .to_vec(),
    )
    .expect("记忆生命周期 SSE 应为 UTF-8")
}

async fn run_memory_contract_turn(
    state: &Arc<AppState>,
    conversation_id: &str,
    client_request_id: &str,
    message: &str,
) -> String {
    let response = api_routes()
        .with_state(Arc::clone(state))
        .oneshot(memory_contract_request(
            conversation_id,
            client_request_id,
            message,
        ))
        .await
        .expect("自动记忆合同请求应返回 SSE");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_sse_text(response).await;
    wait_until_runtime_idle(state).await;
    assert!(body.contains("\"type\":\"done\""));
    body
}

async fn reset_memory_contract_session(state: &Arc<AppState>) -> String {
    let response = api_routes()
        .with_state(Arc::clone(state))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/reset")
                .body(Body::empty())
                .expect("应能构造记忆合同新会话请求"),
        )
        .await
        .expect("记忆合同新会话请求应成功");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    payload["conversation_id"]
        .as_str()
        .expect("新会话响应应包含 conversation_id")
        .to_string()
}

async fn memory_tool_session_payloads(
    state: &Arc<AppState>,
    conversation_ids: &[String],
) -> String {
    let store = state
        .runtime_service
        .session_store()
        .await
        .expect("应打开自动记忆合同 Session Store");
    let mut payloads = Vec::new();
    for conversation_id in conversation_ids {
        let records = store
            .events_for_conversation(conversation_id)
            .await
            .expect("应读取自动记忆合同 Session 事件");
        payloads.extend(
            records
                .into_iter()
                .filter(|record| matches!(record.kind.as_str(), "tool_call" | "tool_result"))
                .map(|record| record.payload),
        );
    }
    assert!(
        !payloads.is_empty(),
        "自动记忆合同必须产生 Tool Session 事件"
    );
    serde_json::to_string(&payloads).expect("应序列化自动记忆合同 Tool Session 事件")
}

async fn wait_until_runtime_idle(state: &AppState) {
    for _ in 0..100 {
        if state
            .runtime_service
            .snapshot()
            .is_ok_and(|snapshot| snapshot.phase == muse_runtime::coordinator::RuntimePhase::Idle)
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("运行时未在预期时间内回到 Idle");
}

fn build_test_state_with_persona(
    config_dir: &Path,
    seed_persona: Option<Persona>,
) -> Arc<AppState> {
    let mut tools = ToolRegistry::new();
    builtin::register_all(&mut tools);
    let mut personas = PersonaStore::load_from_dir(config_dir).expect("应能加载测试角色库");
    if let Some(persona) = seed_persona
        && personas.active_persona_id().is_none()
    {
        if personas.get(&persona.id).is_none() {
            personas.create(persona.clone()).expect("应能创建测试角色");
        }
        personas.set_active(&persona.id).expect("应能激活测试角色");
    }
    let shared_config = Arc::new(Mutex::new(
        muse_core::app::preferences::MuseConfigStore::load_from_dir(config_dir)
            .expect("应能加载测试用户配置"),
    ));
    Arc::new(AppState {
        config: Config::default(),
        provider: Mutex::new(Some(Arc::new(ScriptedChatProvider {
            calls: AtomicUsize::new(0),
        }))),
        tts_provider: Mutex::new(None),
        speech_recognition_provider: Mutex::new(None),
        model_config: Arc::clone(&shared_config),
        user_config: shared_config,
        model_configuration_transition_gate: Mutex::new(()),
        runtime_service: muse_runtime::service::RuntimeService::new_with_data_dir(
            "default",
            Conversation::new("test".to_string(), 10),
            config_dir,
        ),
        personas: Mutex::new(personas),
        visual_packs: Mutex::new(
            VisualPackStore::load_from_dir(config_dir).expect("应能加载测试展示包"),
        ),
        persona_runtime_transition_gate: Mutex::new(()),
        tools,
        mutating_tool_gate: Mutex::new(()),
        chat_request_ids: Mutex::new(crate::state::ChatRequestRegistry::in_memory()),
        emotion_tx: broadcast::channel(1).0,
        memory: None,
    })
}

fn test_persona(id: &str) -> Persona {
    Persona {
        id: id.to_string(),
        name: format!("测试角色 {id}"),
        summary: "路由测试角色".to_string(),
        character_profile: "冷静、可靠".to_string(),
        world_profile: "测试世界".to_string(),
        scenario: String::new(),
        system_prompt: "保持测试角色设定。".to_string(),
        style: "简洁".to_string(),
        roleplay_style: RoleplayStyle::LightNarration,
        dialogue_examples: String::new(),
        author_note: String::new(),
        opening_message: String::new(),
        tool_policy: ToolPolicy::default(),
        skill_policy: Default::default(),
        mcp_policy: Default::default(),
        preferred_model_ref: None,
        preferred_voice_id: None,
        feature_policy: Default::default(),
        default_visual_pack_id: "default-visual-pack".to_string(),
        author: "test".to_string(),
        version: "1.0.0".to_string(),
        notes: String::new(),
    }
}

fn test_visual_pack(id: &str, theme_color: &str) -> VisualPack {
    VisualPack {
        id: id.to_string(),
        name: format!("测试视觉包 {id}"),
        portrait_path: format!("/assets/{id}.png"),
        background_path: format!("/assets/{id}-background.png"),
        avatar_path: format!("/assets/{id}-avatar.png"),
        theme_color: theme_color.to_string(),
        theme_mode: "dark".to_string(),
        layout_mode: "portrait-right".to_string(),
        portrait_frame: "portrait".to_string(),
        portrait_fit: "cover".to_string(),
        portrait_position_x: 50,
        portrait_position_y: 50,
        portrait_scale: 100,
        fallback_text: "测试视觉资源缺失".to_string(),
        version: "1.0.0".to_string(),
        notes: String::new(),
    }
}

struct MuseDataDirEnvGuard {
    previous: Option<std::ffi::OsString>,
}

impl Drop for MuseDataDirEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("MUSE_DATA_DIR", value),
                None => std::env::remove_var("MUSE_DATA_DIR"),
            }
        }
    }
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取 JSON 响应体");
    serde_json::from_slice(&body).expect("响应体应为 JSON")
}

fn assert_flat_api_error(payload: &serde_json::Value) {
    assert!(
        payload["code"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        payload["message"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(payload["field_errors"].is_object());
    assert!(payload["retryable"].is_boolean());
    assert!(
        payload["request_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("req-"))
    );
    assert!(payload.get("api_error").is_none());
    assert!(payload.get("details").is_none());
}

async fn wait_until_persona_transition_gate_is_locked(state: &Arc<AppState>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state.persona_runtime_transition_gate.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("角色运行时 transition gate 应在期限内被请求占用");
}

async fn wait_until_runtime_exclusive_operation(state: &Arc<AppState>, operation: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = state
                .runtime_service
                .snapshot()
                .expect("应能读取运行时快照");
            if snapshot.exclusive_operation.as_deref() == Some(operation) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("运行时操作 `{operation}` 应在期限内取得独占租约"));
}

#[tokio::test]
async fn active_persona_endpoint_returns_nullable_fact_response() {
    let config_dir = unique_temp_dir("active-persona-empty");
    let app = api_routes().with_state(build_empty_test_state(&config_dir));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/personas/active")
                .body(Body::empty())
                .expect("应能构造活动角色查询"),
        )
        .await
        .expect("活动角色查询应成功");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert!(payload["active_persona"].is_null());
    assert!(payload["active_persona_id"].is_null());
    assert!(payload["visual_pack"].is_null());
    assert!(payload["state_revision"].is_u64());
    assert_eq!(payload.as_object().map(serde_json::Map::len), Some(4));
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn approval_mode_route_exposes_three_axes_and_rejects_stale_revision() {
    let config_dir = unique_temp_dir("approval-mode-route");
    let app = api_routes().with_state(build_test_state(&config_dir));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/approval-mode")
                .body(Body::empty())
                .expect("应构造审批模式查询"),
        )
        .await
        .expect("审批模式查询应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let initial = response_json(response).await;
    assert_eq!(initial["preset"], "manual");
    assert_eq!(initial["approval_policy"], "on_request");
    assert_eq!(initial["approvals_reviewer"], "user");
    assert_eq!(initial["permission_profile"], "workspace_write");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/runtime/approval-mode")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "preset": "auto",
                        "expected_revision": initial["revision"]
                    })
                    .to_string(),
                ))
                .expect("应构造 AUTO 模式更新"),
        )
        .await
        .expect("AUTO 模式更新应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let auto = response_json(response).await;
    assert_eq!(auto["preset"], "auto");
    assert_eq!(auto["approval_policy"], "on_request");
    assert_eq!(auto["approvals_reviewer"], "auto_review");
    assert_eq!(auto["permission_profile"], "workspace_write");
    assert!(auto["revision"].as_u64() > initial["revision"].as_u64());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/runtime/approval-mode")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "preset": "yolo",
                        "expected_revision": initial["revision"]
                    })
                    .to_string(),
                ))
                .expect("应构造过期 revision 更新"),
        )
        .await
        .expect("过期 revision 更新应返回响应");
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/runtime/approval-mode")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "preset": "yolo",
                        "expected_revision": auto["revision"]
                    })
                    .to_string(),
                ))
                .expect("应构造 YOLO 模式更新"),
        )
        .await
        .expect("YOLO 模式更新应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let yolo = response_json(response).await;
    assert_eq!(yolo["preset"], "yolo");
    assert_eq!(yolo["approval_policy"], "never");
    assert_eq!(yolo["approvals_reviewer"], "user");
    assert_eq!(yolo["permission_profile"], "danger_full_access");
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn workspace_defaults_reject_persistent_yolo_policy() {
    let config_dir = unique_temp_dir("workspace-default-yolo");
    let app = api_routes().with_state(build_test_state(&config_dir));

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/runtime/workspaces")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "permission_mode": "full_access",
                        "sandbox_mode": "danger_full_access"
                    })
                    .to_string(),
                ))
                .expect("应构造持久化 YOLO 默认值请求"),
        )
        .await
        .expect("持久化 YOLO 默认值请求应返回响应");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = response_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| { message.contains("YOLO 不能保存为全局默认") })
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn appearance_preferences_route_uses_config_toml_without_losing_extensions() {
    let config_dir = unique_temp_dir("appearance-preferences");
    std::fs::create_dir_all(&config_dir).expect("应创建测试数据目录");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"# 保留注释
schema_version = 1
extension_flag = "keep"

[appearance]
theme = "dark"
language = "zh-CN"
background_blur = 12
background_opacity = 0.9
motion_level = "full"

[conversation]
send_key = "enter"
restore_last_session = true

[voice]
auto_play = false
input_language = "zh"

[updates]
check_on_startup = true
"#,
    )
    .expect("应写入用户配置");
    let app = api_routes().with_state(build_empty_test_state(&config_dir));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/preferences/appearance")
                .body(Body::empty())
                .expect("应构造外观查询"),
        )
        .await
        .expect("外观查询应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["appearance"]["background_blur"], 12);
    assert_eq!(payload["diagnostics"][0]["field_path"], "extension_flag");

    let manually_edited = std::fs::read_to_string(config_dir.join("config.toml"))
        .expect("应读取待手工编辑的配置")
        .replace("background_blur = 12", "background_blur = 14");
    std::fs::write(config_dir.join("config.toml"), &manually_edited).expect("应模拟手工编辑配置");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/preferences/appearance")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "background_blur": 26,
                        "background_opacity": 0.65,
                        "motion_level": "reduced"
                    })
                    .to_string(),
                ))
                .expect("应构造外观更新"),
        )
        .await
        .expect("外观更新应返回响应");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let payload = response_json(response).await;
    assert_eq!(payload["code"], "config_revision_conflict");
    assert_eq!(payload["field_path"], "config.toml");

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/preferences/appearance")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "background_blur": 26,
                        "background_opacity": 0.65,
                        "motion_level": "reduced"
                    })
                    .to_string(),
                ))
                .expect("应构造冲突恢复后的外观更新"),
        )
        .await
        .expect("冲突恢复后的外观更新应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert_eq!(payload["appearance"]["background_blur"], 26);
    assert_eq!(payload["appearance"]["motion_level"], "reduced");
    let content =
        std::fs::read_to_string(config_dir.join("config.toml")).expect("应读取更新后的用户配置");
    assert!(content.starts_with("# 保留注释"));
    assert!(content.contains("extension_flag = \"keep\""));
    assert!(content.contains("theme = \"dark\""));
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn provider_model_list_requires_key_before_network_and_asset_route_is_gone() {
    let config_dir = unique_temp_dir("provider-key-gate");
    let app = api_routes().with_state(build_test_state(&config_dir));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/models/catalog/fetch")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "deepseek",
                        "purpose": "chat",
                        "api_base": "https://api.deepseek.com",
                        "api_key": null
                    })
                    .to_string(),
                ))
                .expect("应能构造模型列表请求"),
        )
        .await
        .expect("模型列表密钥门禁应返回响应");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = response_json(response).await;
    assert_eq!(payload["code"], "provider_api_key_required");

    let retired = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/model-assets")
                .body(Body::empty())
                .expect("应能构造退场路由请求"),
        )
        .await
        .expect("退场路由应返回 404");
    assert_eq!(retired.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn persona_update_removes_only_unreferenced_uploaded_asset() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("persona-asset-cleanup");
    install_empty_test_memory_services(&config_dir);
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let persona_id = "router-test-persona";
    let visual_pack_id = format!("visual-{persona_id}");
    let old_avatar_name = format!("{}.webp", "a".repeat(64));
    let kept_portrait_name = format!("{}.webp", "b".repeat(64));
    let old_background_name = format!("{}.webp", "d".repeat(64));
    let old_avatar_url = format!("/api/assets/uploaded/{old_avatar_name}");
    let kept_portrait_url = format!("/api/assets/uploaded/{kept_portrait_name}");
    let old_background_url = format!("/api/assets/uploaded/{old_background_name}");
    let new_avatar_url = format!("/api/assets/uploaded/{}.webp", "c".repeat(64));
    {
        let mut personas = state.personas.lock().await;
        let mut persona = personas.get(persona_id).cloned().expect("测试角色应存在");
        persona.default_visual_pack_id = visual_pack_id.clone();
        personas.update(persona).expect("应更新角色展示包引用");
        personas.save().expect("应保存角色展示包引用");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        let mut pack = test_visual_pack(&visual_pack_id, "#d8596f");
        pack.portrait_path = kept_portrait_url.clone();
        pack.background_path = old_background_url;
        pack.avatar_path = old_avatar_url.clone();
        visual_packs.upsert(pack).expect("应写入测试展示包");
        visual_packs.save().expect("应保存测试展示包");
    }
    let uploaded_dir = config_dir.join("assets").join("uploaded");
    std::fs::create_dir_all(&uploaded_dir).expect("应创建上传资源目录");
    std::fs::write(uploaded_dir.join(&old_avatar_name), b"old-avatar").expect("应写入待清理头像");
    std::fs::write(uploaded_dir.join(&kept_portrait_name), b"kept-portrait")
        .expect("应写入仍被引用的立绘");
    std::fs::write(uploaded_dir.join(&old_background_name), b"old-background")
        .expect("应写入待清理的旧角色背景");
    let persona = state
        .personas
        .lock()
        .await
        .get(persona_id)
        .cloned()
        .expect("测试角色应存在");
    let app = api_routes().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/personas/{persona_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "persona": persona,
                        "visual_pack_patch": {
                            "portrait_path": kept_portrait_url,
                            "background_path": "",
                            "avatar_path": new_avatar_url
                        }
                    })
                    .to_string(),
                ))
                .expect("应能构造角色图片槽更新请求"),
        )
        .await
        .expect("角色图片槽更新应返回响应");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!uploaded_dir.join(old_avatar_name).exists());
    assert!(!uploaded_dir.join(old_background_name).exists());
    assert!(uploaded_dir.join(&kept_portrait_name).exists());

    let delete_response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/personas/{persona_id}"))
                .body(Body::empty())
                .expect("应能构造角色删除请求"),
        )
        .await
        .expect("角色删除应返回响应");
    assert_eq!(delete_response.status(), StatusCode::OK);
    assert!(!uploaded_dir.join(&kept_portrait_name).exists());
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn asset_rollback_deletes_only_unreferenced_uploaded_asset() {
    let config_dir = unique_temp_dir("asset-upload-rollback");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let referenced_name = format!("{}.webp", "a".repeat(64));
    let orphan_name = format!("{}.webp", "b".repeat(64));
    let referenced_url = format!("/api/assets/uploaded/{referenced_name}");
    {
        let mut visual_packs = state.visual_packs.lock().await;
        let mut pack = test_visual_pack("visual-router-test-persona", "#d8596f");
        pack.portrait_path = referenced_url;
        visual_packs.upsert(pack).expect("应写入测试展示包");
    }
    let uploaded_dir = config_dir.join("assets").join("uploaded");
    std::fs::create_dir_all(&uploaded_dir).expect("应创建上传资源目录");
    std::fs::write(uploaded_dir.join(&referenced_name), b"referenced").expect("应写入已引用资源");
    std::fs::write(uploaded_dir.join(&orphan_name), b"orphan").expect("应写入无引用资源");
    let app = api_routes().with_state(state);

    let orphan_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/assets/uploaded/{orphan_name}"))
                .body(Body::empty())
                .expect("应能构造无引用资源回滚请求"),
        )
        .await
        .expect("无引用资源回滚应返回响应");
    assert_eq!(orphan_response.status(), StatusCode::NO_CONTENT);
    assert!(!uploaded_dir.join(&orphan_name).exists());

    let referenced_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/assets/uploaded/{referenced_name}"))
                .body(Body::empty())
                .expect("应能构造已引用资源回滚请求"),
        )
        .await
        .expect("已引用资源回滚应返回响应");
    assert_eq!(referenced_response.status(), StatusCode::NO_CONTENT);
    assert!(uploaded_dir.join(&referenced_name).exists());

    let invalid_response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/assets/uploaded/invalid.webp")
                .body(Body::empty())
                .expect("应能构造非法资源回滚请求"),
        )
        .await
        .expect("非法资源回滚应返回响应");
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn mcp_management_uses_config_toml_as_the_only_fact_source() {
    let config_dir = unique_temp_dir("mcp-config-toml");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let app = api_routes().with_state(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "docs",
                        "enabled": true,
                        "transport": {
                            "type": "stdio",
                            "command": "docs-mcp",
                            "args": [],
                            "env": {},
                            "secrets": [{
                                "target": "DOCS_TOKEN",
                                "secret": {
                                    "action": "replace",
                                    "value": "router-mcp-secret"
                                }
                            }]
                        }
                    })
                    .to_string(),
                ))
                .expect("应能构造 MCP 创建请求"),
        )
        .await
        .expect("MCP 创建应返回响应");

    assert_eq!(response.status(), StatusCode::CREATED);
    let response_body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应读取 MCP 创建响应");
    let response_json: serde_json::Value =
        serde_json::from_slice(&response_body).expect("MCP 创建响应应为 JSON");
    assert_eq!(
        response_json["transport"]["secrets"][0]["target"],
        "DOCS_TOKEN"
    );
    assert_eq!(response_json["transport"]["secrets"][0]["configured"], true);
    assert!(!response_json.to_string().contains("router-mcp-secret"));

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers/docs")
                .body(Body::empty())
                .expect("应构造 MCP 详情请求"),
        )
        .await
        .expect("MCP 详情应返回响应");
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body = to_bytes(get_response.into_body(), usize::MAX)
        .await
        .expect("应读取 MCP 详情响应");
    assert!(!String::from_utf8_lossy(&get_body).contains("router-mcp-secret"));

    let draft_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers/test-draft")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "include_resources": false,
                        "server": {
                            "name": "draft-only",
                            "request_timeout_ms": 1000,
                            "transport": {
                                "type": "stdio",
                                "command": "/usr/bin/false",
                                "args": [],
                                "env": {},
                                "secrets": []
                            }
                        }
                    })
                    .to_string(),
                ))
                .expect("应构造 MCP 草稿测试请求"),
        )
        .await
        .expect("MCP 草稿测试应返回响应");
    assert_eq!(draft_response.status(), StatusCode::OK);
    let draft_body = to_bytes(draft_response.into_body(), usize::MAX)
        .await
        .expect("应读取 MCP 草稿测试响应");
    let draft_json: serde_json::Value =
        serde_json::from_slice(&draft_body).expect("草稿测试响应应为 JSON");
    assert_eq!(draft_json["server"], "draft-only");
    assert_eq!(draft_json["status"], "failed");

    let saved_test = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers/docs/test")
                .body(Body::empty())
                .expect("应构造已保存 MCP 测试请求"),
        )
        .await
        .expect("已保存 MCP 测试应返回响应");
    assert_eq!(saved_test.status(), StatusCode::OK);
    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/mcp/servers")
                .body(Body::empty())
                .expect("应构造 MCP 列表请求"),
        )
        .await
        .expect("MCP 列表应返回响应");
    let list_body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .expect("应读取 MCP 列表响应");
    let list_json: serde_json::Value =
        serde_json::from_slice(&list_body).expect("MCP 列表响应应为 JSON");
    assert_eq!(list_json[0]["status"], "failed");
    assert_eq!(list_json[0]["tested_revision"], list_json[0]["revision"]);
    assert!(list_json[0]["last_error"]["code"].as_str().is_some());
    let config_store = state.user_config.lock().await;
    assert!(config_store.mcp_profiles().mcp_servers.contains_key("docs"));
    assert!(
        !config_store
            .mcp_profiles()
            .mcp_servers
            .contains_key("draft-only")
    );
    let persisted = std::fs::read_to_string(config_dir.join("config.toml"))
        .expect("config.toml MCP 配置应已落盘");
    assert!(persisted.contains("[mcp_servers.docs]"));
    assert!(persisted.contains("command = \"docs-mcp\""));
    assert!(persisted.contains("DOCS_TOKEN = \"router-mcp-secret\""));
    drop(config_store);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn skill_management_uses_standard_directory_and_toml_enable_state() {
    let config_dir = unique_temp_dir("skill-standard-api");
    let state = build_test_state(&config_dir);
    let app = api_routes().with_state(state.clone());

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "中文名称",
                        "description": "无效名称",
                        "content": "正文",
                        "enabled": true
                    })
                    .to_string(),
                ))
                .expect("应构造无效 Skill 请求"),
        )
        .await
        .expect("无效 Skill 请求应返回响应");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(
        response_json(invalid).await["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("skill_invalid："))
    );

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/skills")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "api-helper",
                        "description": "API Skill",
                        "content": "# 初始正文",
                        "enabled": true
                    })
                    .to_string(),
                ))
                .expect("应构造 Skill 创建请求"),
        )
        .await
        .expect("Skill 创建应返回响应");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = response_json(created).await;
    let first_revision = created["revision"].as_str().expect("应返回 revision");
    let skill_dir = config_dir.join("skills/api-helper");
    let document_path = skill_dir.join("SKILL.md");
    let manual = std::fs::read_to_string(&document_path)
        .expect("应读取 Skill 文档")
        .replace(
            "description: \"API Skill\"",
            "description: \"API Skill\"\nlicense: MIT\nmetadata:\n  author: luna",
        );
    std::fs::write(&document_path, manual).expect("应模拟手工扩展 frontmatter");
    std::fs::create_dir_all(skill_dir.join("scripts")).expect("应创建辅助目录");
    std::fs::write(skill_dir.join("scripts/run.sh"), "echo ok").expect("应创建辅助脚本");

    let stale = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/skills/api-helper")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "api-helper",
                        "description": "陈旧更新",
                        "content": "正文",
                        "enabled": true,
                        "revision": first_revision
                    })
                    .to_string(),
                ))
                .expect("应构造陈旧 Skill 请求"),
        )
        .await
        .expect("陈旧 Skill 请求应返回响应");
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let latest = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/skills/api-helper")
                .body(Body::empty())
                .expect("应构造 Skill 详情请求"),
        )
        .await
        .expect("Skill 详情应返回响应");
    let latest = response_json(latest).await;
    let latest_revision = latest["revision"].as_str().expect("应返回最新 revision");
    let updated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/skills/api-helper")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "api-helper-next",
                        "description": "已更新",
                        "content": "# 新正文",
                        "enabled": false,
                        "revision": latest_revision
                    })
                    .to_string(),
                ))
                .expect("应构造 Skill 更新请求"),
        )
        .await
        .expect("Skill 更新应返回响应");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = response_json(updated).await;
    assert_eq!(updated["enabled"], false);
    assert!(
        config_dir
            .join("skills/api-helper-next/scripts/run.sh")
            .is_file()
    );
    let saved = std::fs::read_to_string(config_dir.join("skills/api-helper-next/SKILL.md"))
        .expect("应读取更新后的 Skill");
    assert!(saved.contains("license: MIT"));
    assert!(saved.contains("metadata:\n  author: luna"));
    assert!(!saved.contains("enabled:"));
    let config =
        std::fs::read_to_string(config_dir.join("config.toml")).expect("应读取 Skill 启停配置");
    assert!(config.contains("path = \"skills/api-helper-next/SKILL.md\""));
    assert!(config.contains("enabled = false"));

    let revision = updated["revision"].as_str().expect("应返回更新 revision");
    let deleted = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/skills/api-helper-next?revision={revision}"))
                .body(Body::empty())
                .expect("应构造 Skill 删除请求"),
        )
        .await
        .expect("Skill 删除应返回响应");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(!config_dir.join("skills/api-helper-next").exists());
    let config =
        std::fs::read_to_string(config_dir.join("config.toml")).expect("应回读删除后的 Skill 配置");
    assert!(!config.contains("skills/api-helper-next/SKILL.md"));
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn runtime_skill_catalog_exposes_only_safe_effective_metadata() {
    let config_dir = unique_temp_dir("runtime-skill-catalog-api");
    let state = build_test_state(&config_dir);
    let response = api_routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .uri("/runtime/skills")
                .body(Body::empty())
                .expect("应构造运行时 Skill 目录请求"),
        )
        .await
        .expect("运行时 Skill 目录应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    let creator = payload["skills"]
        .as_array()
        .expect("应返回 Skill 数组")
        .iter()
        .find(|skill| skill["name"] == "skill-creator")
        .expect("有效目录应包含内置 skill-creator");
    assert_eq!(creator["source"], "builtin");
    assert!(
        creator["revision"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );
    assert!(creator.get("content").is_none());
    assert!(creator.get("path").is_none());
    assert_eq!(payload["omitted_skill_count"], 0);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn session_metadata_is_v3_fact_while_export_and_context_keep_public_boundaries() {
    let config_dir = unique_temp_dir("session-metadata-api");
    let state = build_test_state(&config_dir);
    let store = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开测试会话存储");
    store
        .ensure_persona_binding(
            "chapter-1",
            "router-test-persona",
            "测试角色 router-test-persona",
            "1.0.0",
        )
        .await
        .expect("应建立测试会话 v2 Persona 绑定");
    for (kind, payload) in [
        (
            "runtime_policy_snapshot",
            serde_json::json!({
                "conversation_id": "chapter-1",
                "turn_id": "turn-1",
                "snapshot": {
                    "provider": "deepseek",
                    "model": "deepseek-chat",
                    "tool_ids": ["skill"],
                    "mcp_catalog_hash": "public-hash"
                }
            }),
        ),
        (
            "user",
            serde_json::json!({
                "conversation_id": "chapter-1",
                "turn_id": "turn-1",
                "content": "开始第一章"
            }),
        ),
        (
            "assistant",
            serde_json::json!({
                "conversation_id": "chapter-1",
                "turn_id": "turn-1",
                "content": "故事开始。"
            }),
        ),
        (
            "turn_committed",
            serde_json::json!({
                "conversation_id": "chapter-1",
                "turn_id": "turn-1",
                "outcome": "committed"
            }),
        ),
    ] {
        store
            .append_event("chapter-1", Some("turn-1".to_string()), kind, payload)
            .await
            .expect("应能写入测试事件");
    }
    let app = api_routes().with_state(state.clone());
    let patch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/runtime/sessions/chapter-1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"title":"第一章","archived":true}"#))
                .expect("应能构造元数据请求"),
        )
        .await
        .expect("元数据接口应返回响应");
    assert_eq!(patch_response.status(), StatusCode::OK);
    let patch_payload = response_json(patch_response).await;
    assert!(
        patch_payload["revision"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions")
                .body(Body::empty())
                .expect("应能构造会话列表请求"),
        )
        .await
        .expect("会话列表接口应返回响应");
    let list_payload = response_json(list).await;
    let chapter = list_payload["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["conversation_id"] == "chapter-1")
        })
        .expect("列表应包含测试会话");
    assert_eq!(chapter["summary"], "第一章");
    assert_eq!(chapter["archived"], true);

    let export = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions/chapter-1/export")
                .body(Body::empty())
                .expect("应能构造导出请求"),
        )
        .await
        .expect("导出接口应返回响应");
    let export_payload = response_json(export).await;
    assert_eq!(export_payload["title"], "第一章");
    assert_eq!(export_payload["messages"][0]["content"], "开始第一章");
    assert_eq!(export_payload["messages"][1]["content"], "故事开始。");
    let export_text = export_payload.to_string();
    assert!(!export_text.contains("mcp_catalog_hash"));
    assert!(!export_text.contains(config_dir.to_string_lossy().as_ref()));

    let context = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions/chapter-1/context")
                .body(Body::empty())
                .expect("应能构造上下文请求"),
        )
        .await
        .expect("上下文接口应返回响应");
    let context_payload = response_json(context).await;
    assert_eq!(
        context_payload["runtime_policy_snapshot"]["provider"],
        "deepseek"
    );
    let profile = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions/chapter-1/runtime-profile")
                .body(Body::empty())
                .expect("应能构造运行策略请求"),
        )
        .await
        .expect("运行策略接口应返回响应");
    let profile_payload = response_json(profile).await;
    assert_eq!(profile_payload["snapshot"]["model"], "deepseek-chat");
    assert!(!config_dir.join("sessions/metadata.json").exists());
    let events = state
        .runtime_service
        .session_store()
        .await
        .expect("应能重新打开事件存储")
        .events_for_conversation("chapter-1")
        .await
        .expect("应能读取事件");
    assert_eq!(
        events.len(),
        6,
        "v2 绑定和标题元数据都必须是 Session v3 事实事件"
    );
    assert_eq!(
        events.last().map(|event| event.kind.as_str()),
        Some("session_metadata_updated")
    );

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/runtime/sessions/chapter-1")
                .body(Body::empty())
                .expect("应能构造会话删除请求"),
        )
        .await
        .expect("会话删除接口应返回响应");
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted_payload = response_json(deleted).await;
    assert_eq!(deleted_payload["conversation_id"], "chapter-1");
    assert_eq!(deleted_payload["deleted_records"], 6);

    let list_after_delete = app
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions")
                .body(Body::empty())
                .expect("应能构造删除后的会话列表请求"),
        )
        .await
        .expect("删除后的会话列表接口应返回响应");
    let list_after_delete_payload = response_json(list_after_delete).await;
    assert!(
        list_after_delete_payload["sessions"]
            .as_array()
            .is_some_and(|sessions| sessions
                .iter()
                .all(|session| session["conversation_id"] != "chapter-1")),
        "删除后会话不得残留在列表索引中"
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn persona_list_returns_flat_summaries_with_stable_visual_previews() {
    let config_dir = unique_temp_dir("persona-library-visual-preview");
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut portrait_only = test_persona("portrait-only-persona");
        portrait_only.default_visual_pack_id = "portrait-only-pack".to_string();
        personas
            .create(portrait_only)
            .expect("应能创建仅有立绘的角色");

        let mut missing_visual = test_persona("missing-visual-persona");
        missing_visual.default_visual_pack_id = "missing-visual-pack".to_string();
        personas
            .create(missing_visual)
            .expect("应能创建展示包缺失的角色");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(test_visual_pack("default-visual-pack", "#112233"))
            .expect("应能创建与角色匹配的展示包");
        let mut portrait_only_pack = test_visual_pack("portrait-only-pack", "#445566");
        portrait_only_pack.avatar_path = "   ".to_string();
        portrait_only_pack.portrait_path =
            "  /assets/portrait-only-pack-portrait.png  ".to_string();
        visual_packs
            .upsert(portrait_only_pack)
            .expect("空头像不应影响立绘预览");
    }
    let app = api_routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/personas")
                .body(Body::empty())
                .expect("应能构造角色列表查询"),
        )
        .await
        .expect("角色列表查询应成功");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert_eq!(payload["active_persona_id"], "router-test-persona");
    let items = payload["personas"].as_array().expect("角色列表应为数组");
    assert_eq!(items.len(), 3);

    let matched = items
        .iter()
        .find(|item| item["id"] == "router-test-persona")
        .expect("应返回匹配展示包的角色");
    assert_eq!(matched["name"], "测试角色 router-test-persona");
    assert_eq!(matched["summary"], "路由测试角色");
    assert_eq!(matched["default_visual_pack_id"], "default-visual-pack");
    assert_eq!(matched["author"], "test");
    assert_eq!(matched["version"], "1.0.0");
    assert!(matched.get("persona").is_none());
    assert_eq!(
        matched["visual_preview"]["avatar_path"],
        "/assets/default-visual-pack-avatar.png"
    );
    assert_eq!(
        matched["visual_preview"]["portrait_path"],
        "/assets/default-visual-pack.png"
    );

    let portrait_only = items
        .iter()
        .find(|item| item["id"] == "portrait-only-persona")
        .expect("应返回仅有立绘的角色");
    assert!(portrait_only["visual_preview"]["avatar_path"].is_null());
    assert_eq!(
        portrait_only["visual_preview"]["portrait_path"],
        "/assets/portrait-only-pack-portrait.png"
    );

    let missing = items
        .iter()
        .find(|item| item["id"] == "missing-visual-persona")
        .expect("应返回展示包缺失的角色");
    assert!(missing["visual_preview"]["avatar_path"].is_null());
    assert!(missing["visual_preview"]["portrait_path"].is_null());
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn delete_persona_without_memory_services_fails_before_state_changes() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-persona-without-memory-services");
    let state = build_test_state(&config_dir);
    state
        .personas
        .lock()
        .await
        .save()
        .expect("应先保存 Persona 文件基线");
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding(
            "memory-service-missing-chat",
            "router-test-persona",
            "测试角色 router-test-persona",
            "1.0.0",
        )
        .await
        .expect("应能建立待保护的会话绑定");
    repository
        .append_event(
            "memory-service-missing-chat",
            Some("turn-memory-service-missing".to_string()),
            "user",
            serde_json::json!({"content": "记忆服务缺失时不得改变会话"}),
        )
        .await
        .expect("应能写入待保护的会话事件");
    repository
        .set_workspace_state("router-test-persona", "memory-service-missing-chat")
        .expect("应能写入待保护的工作区状态");

    let persona_file = config_dir.join("personas/personas.json");
    let persona_file_before = std::fs::read(&persona_file).expect("应读取 Persona 文件基线");
    let sessions_before = repository
        .list_sessions()
        .await
        .expect("应读取会话列表基线");
    let session_files_before = snapshot_regular_files(&config_dir.join("sessions/conversations"));
    let runtime_snapshot_before = state
        .runtime_service
        .snapshot()
        .expect("应读取运行时事实基线");
    let active_conversation_before = state
        .runtime_service
        .active_conversation_id()
        .expect("应读取活动会话基线");
    let recovery_path = config_dir.join("runtime/persona-deletion-recovery.json");
    assert!(!recovery_path.exists(), "测试开始前不应存在删除恢复记录");

    let response = api_routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造记忆服务缺失的角色删除请求"),
        )
        .await
        .expect("记忆服务缺失应返回稳定失败响应");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let payload = response_json(response).await;
    assert_flat_api_error(&payload);
    assert_eq!(payload["code"], "memory_repository_unavailable");
    let personas = state.personas.lock().await;
    assert!(personas.get("router-test-persona").is_some());
    assert_eq!(personas.active_persona_id(), Some("router-test-persona"));
    drop(personas);
    assert_eq!(
        std::fs::read(&persona_file).expect("应读取拒绝删除后的 Persona 文件"),
        persona_file_before,
        "记忆服务缺失不得改变 Persona 文件"
    );
    assert_eq!(
        repository
            .list_sessions()
            .await
            .expect("应读取拒绝删除后的会话列表"),
        sessions_before,
        "记忆服务缺失不得改变会话事实"
    );
    assert_eq!(
        snapshot_regular_files(&config_dir.join("sessions/conversations")),
        session_files_before,
        "记忆服务缺失不得改写 Session JSONL"
    );
    assert!(
        repository
            .workspace_state_exists("router-test-persona")
            .expect("应读取拒绝删除后的工作区状态"),
        "记忆服务缺失不得清理 Persona 工作区状态"
    );
    assert_eq!(
        state
            .runtime_service
            .snapshot()
            .expect("应读取拒绝删除后的运行时事实"),
        runtime_snapshot_before,
        "记忆服务缺失不得改变活动运行时事实"
    );
    assert_eq!(
        state
            .runtime_service
            .active_conversation_id()
            .expect("应读取拒绝删除后的活动会话"),
        active_conversation_before
    );
    assert!(!recovery_path.exists(), "拒绝删除不得创建 pending 恢复证据");

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn deleting_active_persona_does_not_activate_remaining_persona() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-active-persona");
    install_empty_test_memory_services(&config_dir);
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        personas
            .create(test_persona("remaining-persona"))
            .expect("应能创建保留角色");
    }
    let app = api_routes().with_state(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造角色删除请求"),
        )
        .await
        .expect("角色删除请求应成功");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert!(payload["active_persona"].is_null());
    assert!(payload["active_persona_id"].is_null());
    let mutation_revision = payload["state_revision"]
        .as_u64()
        .expect("删除响应应返回最终 revision");
    let personas = state.personas.lock().await;
    assert!(personas.get("remaining-persona").is_some());
    assert!(personas.active_persona_id().is_none());
    drop(personas);

    let active_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/personas/active")
                .body(Body::empty())
                .expect("应能构造活动角色事实查询"),
        )
        .await
        .expect("活动角色事实查询应成功");
    let active_payload = response_json(active_response).await;
    let runtime_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/state")
                .body(Body::empty())
                .expect("应能构造运行时事实查询"),
        )
        .await
        .expect("运行时事实查询应成功");
    let runtime_payload = response_json(runtime_response).await;
    assert!(active_payload["active_persona"].is_null());
    assert!(runtime_payload["active_persona_id"].is_null());
    assert_eq!(active_payload["state_revision"], mutation_revision);
    assert_eq!(runtime_payload["state_revision"], mutation_revision);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn deleted_persona_sessions_remain_exportable_but_cannot_resume() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("deleted-persona-session");
    install_empty_test_memory_services(&config_dir);
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut persona = personas
            .get("router-test-persona")
            .cloned()
            .expect("待删除角色应存在");
        persona.default_visual_pack_id = "visual-router-test-persona".to_string();
        personas.update(persona).expect("应更新待删除角色展示包");
        personas.save().expect("应保存待删除角色");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(test_visual_pack("visual-router-test-persona", "#d8596f"))
            .expect("应创建待删除角色展示包");
        visual_packs.save().expect("应保存待删除角色展示包");
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding(
            "deleted-persona-chat",
            "router-test-persona",
            "测试角色 router-test-persona",
            "1.0.0",
        )
        .await
        .expect("应能绑定待删除角色会话");
    repository
        .append_event(
            "deleted-persona-chat",
            Some("turn-deleted-persona".to_string()),
            "user",
            serde_json::json!({"content": "删除角色后仍需导出"}),
        )
        .await
        .expect("应写入用户事件");
    let committed = repository
        .append_event(
            "deleted-persona-chat",
            Some("turn-deleted-persona".to_string()),
            "turn_committed",
            serde_json::json!({
                "outcome": "committed",
                "persona_effects": {
                    "schema_version": "muse-persona-effects/v1",
                    "persona_id": "router-test-persona",
                    "emotion": {
                        "emotion": "happy",
                        "intensity": 70,
                        "reason_code": "positive_interaction"
                    }
                }
            }),
        )
        .await
        .expect("应写入提交事件");
    repository
        .project_committed_persona_state(&committed)
        .expect("应写入待删除角色状态投影");
    repository
        .set_workspace_state("router-test-persona", "deleted-persona-chat")
        .expect("应写入待删除角色工作区状态");
    let app = api_routes().with_state(state.clone());

    let impact = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/deletion-impact")
                .body(Body::empty())
                .expect("应能构造删除影响查询"),
        )
        .await
        .expect("删除影响查询应返回响应");
    assert_eq!(impact.status(), StatusCode::OK);
    let impact_payload = response_json(impact).await;
    assert_eq!(impact_payload["associated_session_count"], 1);
    assert_eq!(impact_payload["workspace_state_exists"], true);

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造角色删除请求"),
        )
        .await
        .expect("角色删除请求应返回响应");
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(
        !repository
            .workspace_state_exists("router-test-persona")
            .expect("删除后应清理角色工作区状态")
    );
    assert!(
        state
            .visual_packs
            .lock()
            .await
            .get("visual-router-test-persona")
            .is_none(),
        "删除后不得遗留角色生成的展示包"
    );
    let (_, connection) = muse_core::app::storage::open_runtime_database(&config_dir)
        .expect("应打开删除后的运行时数据库");
    for table in ["persona_state_event", "persona_state_projection"] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE persona_id = 'router-test-persona'"),
                [],
                |row| row.get(0),
            )
            .expect("应核对删除后的 Persona 状态");
        assert_eq!(count, 0, "{table} 不得遗留已删除 Persona");
    }
    drop(connection);

    let export = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runtime/sessions/deleted-persona-chat/export")
                .body(Body::empty())
                .expect("应能构造 missing 角色会话导出请求"),
        )
        .await
        .expect("missing 角色会话导出应返回响应");
    assert_eq!(export.status(), StatusCode::OK);
    let export_payload = response_json(export).await;
    assert_eq!(export_payload["persona_status"], "missing");
    assert_eq!(
        export_payload["persona_name_snapshot"],
        "测试角色 router-test-persona"
    );

    let resume = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/sessions/deleted-persona-chat/resume")
                .body(Body::empty())
                .expect("应能构造 missing 角色会话恢复请求"),
        )
        .await
        .expect("missing 角色会话恢复应返回响应");
    assert_eq!(resume.status(), StatusCode::CONFLICT);
    let resume_payload = response_json(resume).await;
    assert!(
        resume_payload["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("session_persona_missing"))
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[cfg(unix)]
#[tokio::test]
async fn persona_file_write_failure_keeps_persona_memory() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-persona-file-failure");
    let memory_dir = unique_temp_dir("delete-persona-file-failure-memory");
    let memory_repository = install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 1,
            revisions: Vec::new(),
        },
    );
    let (memory_id, _) = seed_test_memory(
        &memory_repository,
        "router-test-persona",
        "角色文件失败时必须保留",
    );
    let state = build_test_state(&config_dir);
    state
        .personas
        .lock()
        .await
        .save()
        .expect("应先保存 Persona 文件以注入目录故障");
    let persona_dir = config_dir.join("personas");
    std::fs::set_permissions(&persona_dir, std::fs::Permissions::from_mode(0o500))
        .expect("应注入 Persona 目录只读故障");
    let response = api_routes()
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应构造角色文件失败请求"),
        )
        .await
        .expect("角色文件失败应返回响应");
    std::fs::set_permissions(&persona_dir, std::fs::Permissions::from_mode(0o700))
        .expect("应恢复 Persona 目录权限");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        state
            .personas
            .lock()
            .await
            .get("router-test-persona")
            .is_some(),
        "Persona 提交前失败时内存角色必须保留"
    );
    assert!(
        memory_repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("应读取 Persona 文件失败后的记忆")
            .is_some(),
        "Persona 提交前失败不得清理记忆"
    );

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[cfg(unix)]
#[tokio::test]
async fn visual_pack_write_failure_rolls_back_persona_deletion() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-persona-visual-pack-failure");
    let memory_dir = unique_temp_dir("delete-persona-visual-pack-failure-memory");
    let memory_repository = install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 1,
            revisions: Vec::new(),
        },
    );
    let (memory_id, _) = seed_test_memory(
        &memory_repository,
        "router-test-persona",
        "展示包失败时必须保留",
    );
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut persona = personas
            .get("router-test-persona")
            .cloned()
            .expect("待删除角色应存在");
        persona.default_visual_pack_id = "visual-router-test-persona".to_string();
        personas.update(persona).expect("应更新待删除角色");
        personas.save().expect("应保存待删除角色");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(test_visual_pack("visual-router-test-persona", "#d8596f"))
            .expect("应创建待删除角色展示包");
        visual_packs.save().expect("应保存待删除角色展示包");
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应打开会话仓储");
    repository
        .set_workspace_state("router-test-persona", "default")
        .expect("应写入待删除工作区状态");
    let visual_pack_dir = config_dir.join("visual_packs");
    std::fs::set_permissions(&visual_pack_dir, std::fs::Permissions::from_mode(0o500))
        .expect("应注入展示包目录只读故障");
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应构造角色删除请求"),
        )
        .await
        .expect("删除请求应返回失败响应");
    std::fs::set_permissions(&visual_pack_dir, std::fs::Permissions::from_mode(0o700))
        .expect("应恢复展示包目录权限");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        state
            .personas
            .lock()
            .await
            .get("router-test-persona")
            .is_some(),
        "展示包写入失败时内存角色不得消失"
    );
    assert!(
        PersonaStore::load_from_dir(&config_dir)
            .expect("应重载角色存储")
            .get("router-test-persona")
            .is_some(),
        "展示包写入失败时 personas.json 必须回滚"
    );
    assert!(
        VisualPackStore::load_from_dir(&config_dir)
            .expect("应重载展示包存储")
            .get("visual-router-test-persona")
            .is_some(),
        "展示包写入失败时原展示包必须保留"
    );
    assert!(
        repository
            .workspace_state_exists("router-test-persona")
            .expect("应读取回滚后的工作区状态"),
        "展示包写入失败不得提前清理 SQLite"
    );
    assert!(
        memory_repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("应读取展示包失败后的记忆")
            .is_some(),
        "展示包失败并回滚 Persona 时不得清理记忆"
    );
    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn sqlite_cleanup_failure_rolls_back_persona_and_visual_pack() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-persona-sqlite-failure");
    let memory_dir = unique_temp_dir("delete-persona-sqlite-failure-memory");
    let memory_repository = install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 1,
            revisions: Vec::new(),
        },
    );
    let (memory_id, _) = seed_test_memory(
        &memory_repository,
        "router-test-persona",
        "SQLite 失败时必须保留",
    );
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut persona = personas
            .get("router-test-persona")
            .cloned()
            .expect("待删除角色应存在");
        persona.default_visual_pack_id = "visual-router-test-persona".to_string();
        personas.update(persona).expect("应更新待删除角色");
        personas.save().expect("应保存待删除角色");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(test_visual_pack("visual-router-test-persona", "#d8596f"))
            .expect("应创建待删除角色展示包");
        visual_packs.save().expect("应保存待删除角色展示包");
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应打开会话仓储");
    repository
        .set_workspace_state("router-test-persona", "default")
        .expect("应写入待删除工作区状态");
    let (_, connection) = muse_core::app::storage::open_runtime_database(&config_dir)
        .expect("应打开故障注入前数据库");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("应截断故障注入前 WAL");
    drop(connection);
    let database_path = config_dir.join("runtime/muse.sqlite");
    let database_backup = config_dir.join("runtime/muse.sqlite.test-backup");
    std::fs::rename(&database_path, &database_backup).expect("应暂存运行时数据库");
    std::fs::create_dir(&database_path).expect("应注入 SQLite 非普通文件故障");
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应构造角色删除请求"),
        )
        .await
        .expect("删除请求应返回失败响应");
    std::fs::remove_dir(&database_path).expect("应移除 SQLite 故障占位目录");
    std::fs::rename(&database_backup, &database_path).expect("应恢复运行时数据库");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        state
            .personas
            .lock()
            .await
            .get("router-test-persona")
            .is_some(),
        "SQLite 清理失败时内存角色不得消失"
    );
    assert!(
        PersonaStore::load_from_dir(&config_dir)
            .expect("应重载角色存储")
            .get("router-test-persona")
            .is_some(),
        "SQLite 清理失败时 personas.json 必须回滚"
    );
    assert!(
        VisualPackStore::load_from_dir(&config_dir)
            .expect("应重载展示包存储")
            .get("visual-router-test-persona")
            .is_some(),
        "SQLite 清理失败时展示包必须回滚"
    );
    assert!(
        repository
            .workspace_state_exists("router-test-persona")
            .expect("应读取回滚后的工作区状态"),
        "SQLite 事务失败时工作区状态必须保持"
    );
    assert!(
        memory_repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("应读取 SQLite 失败后的记忆")
            .is_some(),
        "SQLite 清理失败并回滚 Persona 时不得清理记忆"
    );
    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn deleting_inactive_persona_returns_active_persona_visual_snapshot() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("delete-inactive-persona");
    install_empty_test_memory_services(&config_dir);
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut inactive_persona = test_persona("inactive-persona");
        inactive_persona.default_visual_pack_id = "inactive-visual-pack".to_string();
        personas
            .create(inactive_persona)
            .expect("应能创建非活动角色");
    }
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(test_visual_pack("default-visual-pack", "#112233"))
            .expect("应能创建活动角色视觉包");
        visual_packs
            .upsert(test_visual_pack("inactive-visual-pack", "#ddeeff"))
            .expect("应能创建非活动角色视觉包");
    }
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/inactive-persona")
                .body(Body::empty())
                .expect("应能构造非活动角色删除请求"),
        )
        .await
        .expect("非活动角色删除请求应成功");

    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert_eq!(payload["affected_persona"]["id"], "inactive-persona");
    assert_eq!(payload["active_persona"]["id"], "router-test-persona");
    assert_eq!(payload["active_persona_id"], "router-test-persona");
    assert_eq!(payload["visual_pack"]["id"], "default-visual-pack");
    assert_eq!(payload["visual_pack"]["theme_color"], "#112233");
    assert_eq!(
        payload["active_persona"]["default_visual_pack_id"],
        payload["visual_pack"]["id"]
    );
    assert_eq!(payload["runtime_reset"], false);
    let personas = state.personas.lock().await;
    assert!(personas.get("inactive-persona").is_none());
    assert_eq!(personas.active_persona_id(), Some("router-test-persona"));
    drop(personas);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn creating_and_activating_first_persona_is_one_atomic_mutation() {
    let config_dir = unique_temp_dir("create-and-activate-persona");
    let state = build_empty_test_state(&config_dir);
    let conversation_before = state
        .runtime_service
        .active_conversation_id()
        .expect("应能读取创建前会话 ID");
    let persona = test_persona("first-user-persona");
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/personas")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "persona": persona,
                        "visual_pack_patch": null,
                        "activate_after_create": true
                    })
                    .to_string(),
                ))
                .expect("应能构造创建并启用请求"),
        )
        .await
        .expect("创建并启用请求应成功");

    assert_eq!(response.status(), StatusCode::CREATED);
    let payload = response_json(response).await;
    assert_eq!(payload["affected_persona"]["id"], "first-user-persona");
    assert_eq!(payload["active_persona"]["id"], "first-user-persona");
    assert_eq!(payload["active_persona_id"], "first-user-persona");
    assert_eq!(payload["runtime_reset"], true);
    assert_ne!(payload["conversation_id"], conversation_before);
    let personas = state.personas.lock().await;
    assert_eq!(personas.personas().len(), 1);
    assert_eq!(personas.active_persona_id(), Some("first-user-persona"));
    drop(personas);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn chat_without_persona_is_rejected_without_runtime_side_effects() {
    let config_dir = unique_temp_dir("chat-persona-required");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_empty_test_state(&config_dir);
    let provider = Arc::new(CountingChatProvider {
        calls: AtomicUsize::new(0),
    });
    *state.provider.lock().await = Some(provider.clone());
    let before_runtime = state
        .runtime_service
        .snapshot()
        .expect("应能读取运行时快照");
    let before_messages = {
        let conversation = state.runtime_service.lock_conversation().await;
        serde_json::to_value(&conversation.messages).expect("应能序列化初始会话")
    };
    let app = api_routes().with_state(state.clone());
    let request_body = serde_json::json!({
        "message": "不应发送到模型",
        "conversation_id": "default",
        "client_request_id": "persona-required-request"
    })
    .to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body.clone()))
                .expect("应能构造空角色聊天请求"),
        )
        .await
        .expect("空角色聊天请求应被结构化拒绝");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let payload = response_json(response).await;
    assert_eq!(payload["code"], "persona_required");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        state
            .runtime_service
            .snapshot()
            .expect("应能读取拒绝后快照"),
        before_runtime
    );
    let after_messages = {
        let conversation = state.runtime_service.lock_conversation().await;
        serde_json::to_value(&conversation.messages).expect("应能序列化拒绝后会话")
    };
    assert_eq!(after_messages, before_messages);
    let session_store = state
        .runtime_service
        .session_store()
        .await
        .expect("应能读取会话存储");
    assert!(
        session_store
            .events_for_conversation("default")
            .await
            .expect("应能查询 transcript 事件")
            .is_empty()
    );
    let usage_store = muse_core::domain::usage::RuntimeUsageStore::load_from_dir(&config_dir)
        .expect("应能读取用量存储");
    assert!(
        usage_store
            .list_token_usage(Some("default"), None)
            .expect("应能查询 Token 用量")
            .is_empty()
    );
    assert!(
        usage_store
            .latest_context_snapshot("default")
            .expect("应能查询上下文快照")
            .is_none()
    );

    // 使用同一个 request id 再次发送，验证空角色拒绝没有登记幂等事实。
    {
        let mut personas = state.personas.lock().await;
        let persona = test_persona("activated-after-rejection");
        personas
            .create(persona.clone())
            .expect("应能创建后续测试角色");
        personas
            .set_active(&persona.id)
            .expect("应能激活后续测试角色");
    }
    let accepted = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body))
                .expect("应能构造激活后的聊天请求"),
        )
        .await
        .expect("激活后同一请求 id 应可受理");
    assert_eq!(accepted.status(), StatusCode::OK);
    let _ = to_bytes(accepted.into_body(), usize::MAX)
        .await
        .expect("应能等待激活后的流式聊天完成");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn cross_persona_chat_is_rejected_before_provider_and_business_events() {
    let config_dir = unique_temp_dir("chat-persona-mismatch");
    let state = build_test_state(&config_dir);
    let provider = Arc::new(CountingChatProvider {
        calls: AtomicUsize::new(0),
    });
    *state.provider.lock().await = Some(provider.clone());
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding("default", "another-persona", "另一个角色", "1.0.0")
        .await
        .expect("应能模拟其他角色已绑定会话");
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "不应进入模型",
                        "conversation_id": "default",
                        "client_request_id": "persona-mismatch-request"
                    })
                    .to_string(),
                ))
                .expect("应能构造跨角色聊天请求"),
        )
        .await
        .expect("跨角色聊天应返回结构化流错误");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应读取跨角色聊天响应");
    assert!(String::from_utf8_lossy(&body).contains("session_persona_mismatch"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let events = repository
        .session_store()
        .events_for_conversation("default")
        .await
        .expect("应读取被拒会话事件");
    assert_eq!(events.len(), 2, "拒绝请求只允许追加无副作用的终止审计事件");
    assert_eq!(events[0].kind, "session_metadata_updated");
    assert_eq!(events[1].kind, "turn_aborted");
    assert!(events.iter().all(|event| event.kind != "user"));
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn resume_switches_to_bound_persona_and_records_workspace_state() {
    let config_dir = unique_temp_dir("resume-bound-persona");
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        personas
            .create(test_persona("resume-persona"))
            .expect("应能创建会话所属角色");
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding(
            "resume-chat",
            "resume-persona",
            "测试角色 resume-persona",
            "1.0.0",
        )
        .await
        .expect("应能绑定恢复会话");
    repository
        .append_event(
            "resume-chat",
            Some("turn-resume".to_string()),
            "user",
            serde_json::json!({"content": "恢复到指定角色"}),
        )
        .await
        .expect("应写入恢复会话用户事件");
    repository
        .append_event(
            "resume-chat",
            Some("turn-resume".to_string()),
            "turn_committed",
            serde_json::json!({"outcome": "committed"}),
        )
        .await
        .expect("应写入恢复会话提交事件");
    let app = api_routes().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/sessions/resume-chat/resume")
                .body(Body::empty())
                .expect("应能构造会话恢复请求"),
        )
        .await
        .expect("恢复请求应返回响应");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = response_json(response).await;
    assert_eq!(payload["persona_id"], "resume-persona");
    assert_eq!(
        state.personas.lock().await.active_persona_id(),
        Some("resume-persona")
    );
    assert_eq!(
        state
            .runtime_service
            .active_conversation_id()
            .expect("应读取活动会话"),
        "resume-chat"
    );
    assert!(
        repository
            .workspace_state_exists("resume-persona")
            .expect("应读取角色工作区状态")
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn startup_restores_active_persona_workspace_session() {
    let config_dir = unique_temp_dir("startup-persona-workspace");
    let state = build_test_state(&config_dir);
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding(
            "startup-chat",
            "router-test-persona",
            "测试角色 router-test-persona",
            "1.0.0",
        )
        .await
        .expect("应能绑定启动恢复会话");
    repository
        .append_event(
            "startup-chat",
            Some("turn-startup".to_string()),
            "user",
            serde_json::json!({"content": "启动后继续"}),
        )
        .await
        .expect("应写入启动恢复用户事件");
    repository
        .append_event(
            "startup-chat",
            Some("turn-startup".to_string()),
            "turn_committed",
            serde_json::json!({"outcome": "committed"}),
        )
        .await
        .expect("应写入启动恢复提交事件");
    repository
        .set_workspace_state("router-test-persona", "startup-chat")
        .expect("应保存启动工作区会话");

    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("启动恢复应成功");
    assert_eq!(
        state
            .runtime_service
            .active_conversation_id()
            .expect("应读取启动后的活动会话"),
        "startup-chat"
    );
    let messages = state.runtime_service.lock_conversation().await;
    assert!(
        messages
            .messages
            .iter()
            .any(|message| message.content == "启动后继续")
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn startup_keeps_pending_persona_deletion_without_memory_services() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("startup-finish-persona-deletion");
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        let mut deleted_persona = test_persona("deleted-persona");
        deleted_persona.default_visual_pack_id = "visual-deleted-persona".to_string();
        personas
            .create(deleted_persona)
            .expect("应创建待模拟中断删除的 Persona");
        personas.save().expect("应保存待模拟中断删除的 Persona");
    }
    let deleted_visual_pack = test_visual_pack("visual-deleted-persona", "#d8596f");
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(deleted_visual_pack.clone())
            .expect("应创建中断删除遗留的展示包");
        visual_packs
            .upsert(test_visual_pack("visual-user-unreferenced", "#112233"))
            .expect("应创建不属于删除恢复记录的未引用展示包");
        visual_packs.save().expect("应保存中断删除遗留的展示包");
    }
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应能打开会话仓储");
    repository
        .ensure_persona_binding(
            "deleted-persona-chat",
            "deleted-persona",
            "已删除角色",
            "1.0.0",
        )
        .await
        .expect("应保留已删除角色的 Session 绑定");
    let committed = repository
        .append_event(
            "deleted-persona-chat",
            Some("turn-deleted-persona".to_string()),
            "turn_committed",
            serde_json::json!({
                "persona_effects": {
                    "schema_version": "muse-persona-effects/v1",
                    "persona_id": "deleted-persona",
                    "emotion": {
                        "emotion": "happy",
                        "intensity": 70,
                        "reason_code": "positive_interaction"
                    }
                }
            }),
        )
        .await
        .expect("应写入已删除角色的 Session 事实");
    repository
        .project_committed_persona_state(&committed)
        .expect("应模拟中断删除前的状态投影");
    repository
        .set_workspace_state("deleted-persona", "deleted-persona-chat")
        .expect("应模拟中断删除前的工作区状态");
    crate::runtime_support::persist_persona_deletion_recovery_for_test(
        &config_dir,
        "deleted-persona",
        Some(deleted_visual_pack),
        Some("pending-memory-delete"),
    )
    .expect("应写入删除提交前的恢复记录");
    {
        let mut personas = state.personas.lock().await;
        assert!(personas.delete("deleted-persona"));
        personas.save().expect("应模拟 Persona 定义已经提交删除");
    }

    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("记忆服务缺失时应保留恢复证据且不阻断启动");

    assert!(
        state
            .visual_packs
            .lock()
            .await
            .get("visual-deleted-persona")
            .is_some(),
        "记忆服务缺失时不得推进到展示包收敛完成"
    );
    assert!(
        state
            .visual_packs
            .lock()
            .await
            .get("visual-user-unreferenced")
            .is_some(),
        "启动收敛不得按 visual- 前缀误删恢复记录之外的展示包"
    );
    assert!(
        !repository
            .workspace_state_exists("deleted-persona")
            .expect("应读取启动后的工作区状态"),
        "启动后不得遗留已删除 Persona 的工作区状态"
    );
    let (_, connection) = muse_core::app::storage::open_runtime_database(&config_dir)
        .expect("应打开启动后的运行时数据库");
    for table in ["persona_state_event", "persona_state_projection"] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE persona_id = 'deleted-persona'"),
                [],
                |row| row.get(0),
            )
            .expect("应核对启动后的 Persona 状态");
        assert_eq!(count, 0, "启动后 {table} 不得复活已删除 Persona");
    }
    assert!(
        repository
            .list_sessions()
            .await
            .expect("启动后应保留关联 Session")
            .iter()
            .any(|session| session.conversation_id == "deleted-persona-chat"),
        "关联 Session 必须继续作为只读历史保留"
    );
    let recovery: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config_dir.join("runtime/persona-deletion-recovery.json"))
            .expect("记忆服务缺失后必须保留 pending 恢复证据"),
    )
    .expect("pending 恢复证据应保持可解析");
    assert_eq!(recovery["pending"]["persona_id"], "deleted-persona");
    assert_eq!(
        recovery["pending"]["memory_delete_operation_id"],
        "pending-memory-delete"
    );
    drop(connection);
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn startup_keeps_legacy_pending_persona_deletion_without_operation_id() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("startup-legacy-persona-deletion");
    install_empty_test_memory_services(&config_dir);
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        assert!(personas.delete("router-test-persona"));
        personas.save().expect("应模拟旧版 Persona 已提交删除");
    }
    crate::runtime_support::persist_persona_deletion_recovery_for_test(
        &config_dir,
        "router-test-persona",
        None,
        None,
    )
    .expect("应写入缺少 operation ID 的旧版恢复记录");

    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("旧版恢复记录应稳定保留而不是阻断启动");

    let recovery: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config_dir.join("runtime/persona-deletion-recovery.json"))
            .expect("缺少 operation ID 时必须保留 pending 恢复证据"),
    )
    .expect("旧版 pending 恢复证据应保持可解析");
    assert_eq!(recovery["pending"]["persona_id"], "router-test-persona");
    assert!(recovery["pending"]["memory_delete_operation_id"].is_null());

    let _ = std::fs::remove_dir_all(config_dir);
}

#[cfg(unix)]
#[tokio::test]
async fn optional_visual_recovery_failure_does_not_block_startup_and_retries() {
    use std::os::unix::fs::PermissionsExt;

    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("startup-optional-visual-retry");
    install_empty_test_memory_services(&config_dir);
    let state = build_test_state(&config_dir);
    let deleted_visual_pack = test_visual_pack("visual-deleted-persona", "#d8596f");
    {
        let mut visual_packs = state.visual_packs.lock().await;
        visual_packs
            .upsert(deleted_visual_pack.clone())
            .expect("应创建中断删除遗留的展示包");
        visual_packs.save().expect("应保存中断删除遗留的展示包");
    }
    crate::runtime_support::persist_persona_deletion_recovery_for_test(
        &config_dir,
        "deleted-persona",
        Some(deleted_visual_pack),
        Some("optional-visual-memory-delete"),
    )
    .expect("应写入待重试的删除恢复记录");
    let visual_pack_dir = config_dir.join("visual_packs");
    std::fs::set_permissions(&visual_pack_dir, std::fs::Permissions::from_mode(0o500))
        .expect("应注入可选展示包恢复写入故障");

    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("可选展示包恢复失败不得阻断启动");
    assert!(
        state
            .visual_packs
            .lock()
            .await
            .get("visual-deleted-persona")
            .is_some(),
        "写入故障期间应保留展示包等待重试"
    );

    std::fs::set_permissions(&visual_pack_dir, std::fs::Permissions::from_mode(0o700))
        .expect("应恢复展示包目录权限");
    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("下次启动应重试可选展示包恢复");
    assert!(
        state
            .visual_packs
            .lock()
            .await
            .get("visual-deleted-persona")
            .is_none(),
        "故障解除后应完成展示包收敛"
    );
    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn accepted_chat_turn_atomically_wins_concurrent_persona_delete() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("chat-delete-chat-wins");
    install_empty_test_memory_services(&config_dir);
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    *state.provider.lock().await = Some(Arc::new(GatedChatProvider {
        entered: entered.clone(),
        release: release.clone(),
    }));
    let app = api_routes().with_state(state.clone());
    let personas_guard = state.personas.lock().await;

    let chat_app = app.clone();
    let chat_task = tokio::spawn(async move {
        chat_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chat/stream")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "message": "完整受理这个回合",
                            "conversation_id": "default",
                            "client_request_id": "chat-wins-delete-race"
                        })
                        .to_string(),
                    ))
                    .expect("应能构造并发聊天请求"),
            )
            .await
            .expect("并发聊天请求应返回响应")
    });
    wait_until_persona_transition_gate_is_locked(&state).await;

    let delete_app = app.clone();
    let delete_task = tokio::spawn(async move {
        delete_app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/personas/router-test-persona")
                    .body(Body::empty())
                    .expect("应能构造并发角色删除请求"),
            )
            .await
            .expect("并发角色删除请求应返回响应")
    });
    drop(personas_guard);

    let chat_response = chat_task.await.expect("聊天并发任务不应崩溃");
    assert_eq!(chat_response.status(), StatusCode::OK);
    let delete_response = delete_task.await.expect("删除并发任务不应崩溃");
    assert_eq!(delete_response.status(), StatusCode::CONFLICT);
    let delete_payload = response_json(delete_response).await;
    assert_eq!(delete_payload["code"], "runtime_busy");
    entered
        .acquire()
        .await
        .expect("已受理回合应进入 provider")
        .forget();
    release.add_permits(1);
    let body = to_bytes(chat_response.into_body(), usize::MAX)
        .await
        .expect("应能读取完整受理的 SSE");
    assert!(String::from_utf8_lossy(&body).contains("门控测试回复"));
    assert_eq!(
        state.personas.lock().await.active_persona_id(),
        Some("router-test-persona")
    );

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn concurrent_persona_delete_rejects_chat_before_request_id_registration() {
    let _memory_guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("chat-delete-delete-wins");
    install_empty_test_memory_services(&config_dir);
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let provider = Arc::new(CountingChatProvider {
        calls: AtomicUsize::new(0),
    });
    *state.provider.lock().await = Some(provider.clone());
    let app = api_routes().with_state(state.clone());
    let personas_guard = state.personas.lock().await;

    let delete_app = app.clone();
    let delete_task = tokio::spawn(async move {
        delete_app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/personas/router-test-persona")
                    .body(Body::empty())
                    .expect("应能构造优先删除请求"),
            )
            .await
            .expect("优先删除请求应返回响应")
    });
    wait_until_runtime_exclusive_operation(&state, "delete_persona").await;

    let request_id = "delete-wins-chat-race";
    let chat_app = app.clone();
    let chat_task = tokio::spawn(async move {
        chat_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chat/stream")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "message": "删除完成后应拒绝",
                            "conversation_id": "default",
                            "client_request_id": request_id
                        })
                        .to_string(),
                    ))
                    .expect("应能构造等待删除的聊天请求"),
            )
            .await
            .expect("等待删除的聊天请求应返回响应")
    });
    drop(personas_guard);

    let delete_response = delete_task.await.expect("删除并发任务不应崩溃");
    assert_eq!(delete_response.status(), StatusCode::OK);
    let chat_response = chat_task.await.expect("聊天并发任务不应崩溃");
    assert_eq!(chat_response.status(), StatusCode::CONFLICT);
    let chat_payload = response_json(chat_response).await;
    assert_eq!(chat_payload["code"], "persona_required");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    // 拒绝发生在 request ID 登记前；重新激活角色后同一 ID 必须仍可使用。
    {
        let mut personas = state.personas.lock().await;
        let persona = test_persona("persona-after-delete-race");
        personas
            .create(persona.clone())
            .expect("应能创建恢复测试角色");
        personas
            .set_active(&persona.id)
            .expect("应能激活恢复测试角色");
    }
    let retry_conversation_id = state
        .runtime_service
        .active_conversation_id()
        .expect("应能读取删除后的当前会话 ID");
    let retry_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "使用相同请求 ID 重试",
                        "conversation_id": retry_conversation_id,
                        "client_request_id": request_id
                    })
                    .to_string(),
                ))
                .expect("应能构造同 ID 重试请求"),
        )
        .await
        .expect("同 ID 重试应返回响应");
    assert_eq!(retry_response.status(), StatusCode::OK);
    let _ = to_bytes(retry_response.into_body(), usize::MAX)
        .await
        .expect("应能等待同 ID 重试完成");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn persona_gate_blocks_session_and_voice_writes_but_keeps_history_readable() {
    let config_dir = unique_temp_dir("persona-write-gates");
    let state = build_empty_test_state(&config_dir);
    let app = api_routes().with_state(state);

    for (method, uri, body) in [
        ("POST", "/reset", "{}"),
        ("DELETE", "/runtime/sessions/source", "{}"),
        ("POST", "/tts", r#"{"text":"不应合成"}"#),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("应能构造空角色写请求"),
            )
            .await
            .expect("空角色写请求应被结构化拒绝");
        assert_eq!(response.status(), StatusCode::CONFLICT, "写入口：{uri}");
        let payload = response_json(response).await;
        assert_eq!(payload["code"], "persona_required", "写入口：{uri}");
    }

    for uri in ["/history", "/runtime/sessions"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .expect("应能构造空角色只读请求"),
            )
            .await
            .expect("空角色只读请求应保持可用");
        assert_eq!(response.status(), StatusCode::OK, "只读入口：{uri}");
    }

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn chat_stream_emits_structured_tool_status_sequence() {
    let config_dir = unique_temp_dir("chat-status");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let app = api_routes().with_state(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "现在几点",
                        "conversation_id": "default",
                        "client_request_id": "router-test-request"
                    })
                    .to_string(),
                ))
                .expect("应能构造流式聊天请求"),
        )
        .await
        .expect("路由应能处理流式聊天请求");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取流式聊天响应");
    let payloads = String::from_utf8(body.to_vec())
        .expect("SSE 响应应为 UTF-8")
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("SSE data 应为 JSON"))
        .collect::<Vec<_>>();
    let status_payloads = payloads
        .iter()
        .filter(|payload| payload["type"] == "status")
        .collect::<Vec<_>>();
    let phases = status_payloads
        .iter()
        .filter_map(|payload| payload["phase"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        phases,
        vec![
            "queued",
            "thinking",
            "generating",
            "tool_running",
            "tool_completed",
            "synthesizing",
            "generating",
            "completed"
        ]
    );
    assert!(
        status_payloads.iter().all(|payload| {
            payload.get("arguments").is_none() && payload.get("content").is_none()
        })
    );
    assert_eq!(
        payloads.last().map(|payload| &payload["type"]),
        Some(&serde_json::json!("done"))
    );
    let projection = state
        .runtime_service
        .session_repository()
        .await
        .expect("应打开会话仓储")
        .persona_state("router-test-persona")
        .await
        .expect("应读取已提交情绪")
        .expect("最终 assistant 的严格候选应产生长期状态");
    assert_eq!(projection.emotion, "happy");
    assert_eq!(projection.intensity, 70);
    assert_eq!(projection.reason_code, "positive_interaction");
}

#[tokio::test]
async fn memory_mutate_仅在完整提交后落库且错误取消断流与重放均受控() {
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };

    let provider_error_dir = unique_temp_dir("memory-provider-error");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &provider_error_dir);
    }
    let provider_error_calls = Arc::new(AtomicUsize::new(0));
    let provider_error_state =
        build_memory_test_state(&provider_error_dir, Arc::clone(&provider_error_calls));
    *provider_error_state.provider.lock().await = Some(Arc::new(FailingChatProvider));
    let provider_error_response = api_routes()
        .with_state(Arc::clone(&provider_error_state))
        .oneshot(memory_lifecycle_request("memory-provider-error"))
        .await
        .expect("provider 错误仍应返回 SSE");
    let provider_error_body = read_sse_text(provider_error_response).await;
    assert!(provider_error_body.contains("error"));
    assert_eq!(provider_error_calls.load(Ordering::SeqCst), 0);
    wait_until_runtime_idle(&provider_error_state).await;

    let stream_error_dir = unique_temp_dir("memory-stream-error");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &stream_error_dir);
    }
    let stream_error_calls = Arc::new(AtomicUsize::new(0));
    let stream_error_state =
        build_memory_test_state(&stream_error_dir, Arc::clone(&stream_error_calls));
    *stream_error_state.provider.lock().await = Some(Arc::new(MemoryLifecycleProvider {
        calls: AtomicUsize::new(0),
        followup: MemoryProviderFollowup::StreamError,
    }));
    let stream_error_response = api_routes()
        .with_state(Arc::clone(&stream_error_state))
        .oneshot(memory_lifecycle_request("memory-stream-error"))
        .await
        .expect("续写断流仍应返回 SSE");
    let stream_error_body = read_sse_text(stream_error_response).await;
    assert!(stream_error_body.contains("error"));
    assert!(!stream_error_body.contains("memory_commit_completed"));
    assert_eq!(stream_error_calls.load(Ordering::SeqCst), 0);
    wait_until_runtime_idle(&stream_error_state).await;

    let commit_error_dir = unique_temp_dir("memory-session-commit-error");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &commit_error_dir);
    }
    let commit_error_calls = Arc::new(AtomicUsize::new(0));
    let commit_error_state =
        build_memory_test_state(&commit_error_dir, Arc::clone(&commit_error_calls));
    *commit_error_state.provider.lock().await = Some(Arc::new(MemoryLifecycleProvider {
        calls: AtomicUsize::new(0),
        followup: MemoryProviderFollowup::Success,
    }));
    crate::runtime_support::fail_next_turn_commit_for_test("default");
    let commit_error_response = api_routes()
        .with_state(Arc::clone(&commit_error_state))
        .oneshot(memory_lifecycle_request("memory-session-commit-error"))
        .await
        .expect("会话提交失败仍应返回 SSE");
    let commit_error_body = read_sse_text(commit_error_response).await;
    assert!(commit_error_body.contains("error"));
    assert!(!commit_error_body.contains("memory_commit_completed"));
    assert_eq!(commit_error_calls.load(Ordering::SeqCst), 0);
    wait_until_runtime_idle(&commit_error_state).await;

    let cancelled_dir = unique_temp_dir("memory-cancelled");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &cancelled_dir);
    }
    let cancelled_calls = Arc::new(AtomicUsize::new(0));
    let cancelled_state = build_memory_test_state(&cancelled_dir, Arc::clone(&cancelled_calls));
    let pending_entered = Arc::new(Semaphore::new(0));
    *cancelled_state.provider.lock().await = Some(Arc::new(MemoryLifecycleProvider {
        calls: AtomicUsize::new(0),
        followup: MemoryProviderFollowup::Pending(Arc::clone(&pending_entered)),
    }));
    let cancelled_response = api_routes()
        .with_state(Arc::clone(&cancelled_state))
        .oneshot(memory_lifecycle_request("memory-cancelled"))
        .await
        .expect("取消场景应先返回 SSE");
    tokio::time::timeout(std::time::Duration::from_secs(2), pending_entered.acquire())
        .await
        .expect("provider 应在取消前进入续写等待")
        .expect("测试信号量不应关闭")
        .forget();
    drop(cancelled_response);
    wait_until_runtime_idle(&cancelled_state).await;
    assert_eq!(cancelled_calls.load(Ordering::SeqCst), 0);

    let success_dir = unique_temp_dir("memory-success-replay");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &success_dir);
    }
    let success_calls = Arc::new(AtomicUsize::new(0));
    let success_state = build_memory_test_state(&success_dir, Arc::clone(&success_calls));
    *success_state.provider.lock().await = Some(Arc::new(MemoryLifecycleProvider {
        calls: AtomicUsize::new(0),
        followup: MemoryProviderFollowup::Success,
    }));
    let success_app = api_routes().with_state(Arc::clone(&success_state));
    let success_response = success_app
        .clone()
        .oneshot(memory_lifecycle_request("memory-success-replay"))
        .await
        .expect("成功场景应返回 SSE");
    let success_body = read_sse_text(success_response).await;
    assert!(success_body.contains("memory_commit_completed"));
    assert_eq!(success_calls.load(Ordering::SeqCst), 1);
    wait_until_runtime_idle(&success_state).await;

    let replay = success_app
        .oneshot(memory_lifecycle_request("memory-success-replay"))
        .await
        .expect("重复 client_request_id 应稳定拒绝");
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    assert_eq!(success_calls.load(Ordering::SeqCst), 1);

    for dir in [
        provider_error_dir,
        stream_error_dir,
        commit_error_dir,
        cancelled_dir,
        success_dir,
    ] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test]
#[ignore = "仅用于隔离桌面来源跳转验收，需显式提供 MUSE_DESKTOP_SOURCE_FIXTURE_DIR"]
async fn generate_desktop_source_fixture() {
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    let data_dir = PathBuf::from(
        std::env::var_os("MUSE_DESKTOP_SOURCE_FIXTURE_DIR").expect("必须显式提供隔离桌面夹具目录"),
    );
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &data_dir);
    }

    let mut state = build_test_state(&data_dir);
    let persona_id = state
        .personas
        .lock()
        .await
        .active_persona_id()
        .map(str::to_string)
        .expect("隔离桌面夹具必须已有活动 Persona");
    let (runtime_memory, management_memory) =
        crate::runtime_support::build_sqlite_memory_services(&data_dir)
            .expect("应构造生产 SQLite 记忆服务束");
    Arc::get_mut(&mut state)
        .expect("桌面夹具状态尚未共享")
        .memory = Some(runtime_memory);
    drop(management_memory);
    *state.provider.lock().await = Some(Arc::new(MemoryLifecycleProvider {
        calls: AtomicUsize::new(0),
        followup: MemoryProviderFollowup::Success,
    }));
    let conversation_id = state
        .runtime_service
        .active_conversation_id()
        .expect("应读取桌面夹具来源会话 ID");

    let sse = run_memory_contract_turn(
        &state,
        &conversation_id,
        "desktop-source-fixture-request",
        "我喜欢夜间散步",
    )
    .await;
    assert!(sse.contains("memory_commit_completed"));

    let repository =
        SqliteMemoryRepository::open(&data_dir).expect("应打开桌面夹具记忆 Repository");
    let records = repository
        .search_current_fts(&memory_scope(&persona_id), "夜间散步", 5)
        .expect("应查询桌面来源记忆");
    assert_eq!(records.len(), 1, "桌面来源夹具应只生成一条匹配记忆");
    let source = &records[0].current_revision.source;
    assert_eq!(source.conversation_id(), Some(conversation_id.as_str()));
    assert!(source.turn_id().is_some(), "桌面来源夹具必须绑定 Turn");
    println!(
        "桌面来源夹具已生成：persona_id={persona_id} conversation_id={conversation_id} turn_id={}",
        source.turn_id().expect("已核对来源 Turn")
    );
}

#[tokio::test]
async fn automatic_memory_create_query_update_correct_survives_session_and_process_restart() {
    const CREATE_USER_MESSAGE: &str = "我喜欢在晚上喝茉莉花茶";
    const CREATE_CONTENT: &str = "用户偏好在夜间饮用茉莉花茶。";
    const UPDATE_USER_MESSAGE: &str = "我喜欢在晚上喝红茶";
    const UPDATE_CONTENT: &str = "用户现在偏好在夜间饮用红茶。";
    const CORRECT_USER_MESSAGE: &str = "我喜欢在晚上喝无糖茉莉花茶";
    const CORRECT_CONTENT: &str = "用户准确的偏好是在夜间饮用无糖茉莉花茶。";
    const CREATE_REASON: &str = "用户在当前消息中直接说明饮品偏好";
    const UPDATE_REASON: &str = "用户说明饮品偏好后来发生变化";
    const CORRECT_REASON: &str = "用户纠正上一条饮品偏好";

    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    let data_dir = unique_temp_dir("memory-automatic-contract");
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &data_dir);
    }

    let provider = Arc::new(MemoryContractProvider::new());
    let mut state = build_test_state(&data_dir);
    let (runtime_memory, management_memory) =
        crate::runtime_support::build_sqlite_memory_services(&data_dir)
            .expect("应构造生产 SQLite 记忆服务束");
    Arc::get_mut(&mut state).expect("测试状态尚未共享").memory = Some(runtime_memory);
    drop(management_memory);
    *state.provider.lock().await = Some(provider.clone());
    let repository =
        SqliteMemoryRepository::open(&data_dir).expect("应打开自动记忆合同只读核对 Repository");
    let scope = memory_scope("router-test-persona");
    let first_conversation_id = state
        .runtime_service
        .active_conversation_id()
        .expect("应读取首个自动记忆合同会话 ID");
    assert_eq!(first_conversation_id, "default");

    let create_sse = run_memory_contract_turn(
        &state,
        &first_conversation_id,
        "memory-contract-create-request",
        CREATE_USER_MESSAGE,
    )
    .await;
    assert!(create_sse.contains("memory_commit_completed"));
    for forbidden in [CREATE_CONTENT, CREATE_REASON] {
        assert!(
            !create_sse.contains(forbidden),
            "记忆变更 SSE 不得包含正文 `{forbidden}`"
        );
    }
    let mut created_records = repository
        .search_current_fts(&scope, "茉莉花茶", 5)
        .expect("自动 create 后应能查询真实 SQLite");
    assert_eq!(created_records.len(), 1);
    let created = created_records.remove(0);
    assert_eq!(created.current_revision.content, CREATE_CONTENT);
    assert_eq!(
        created.current_revision.source.conversation_id(),
        Some(first_conversation_id.as_str())
    );
    provider.set_current(&created.entry.memory_id, &created.entry.current_revision_id);

    let second_conversation_id = reset_memory_contract_session(&state).await;
    assert_ne!(second_conversation_id, first_conversation_id);
    let first_query_sse = run_memory_contract_turn(
        &state,
        &second_conversation_id,
        "memory-contract-query-cross-session-request",
        "请回忆我的茉莉花茶偏好",
    )
    .await;
    assert!(!first_query_sse.contains(CREATE_CONTENT));
    let first_query_results = provider.query_results_for_model();
    assert_eq!(first_query_results.len(), 1);
    assert!(first_query_results[0].contains(CREATE_CONTENT));

    let update_sse = run_memory_contract_turn(
        &state,
        &second_conversation_id,
        "memory-contract-update-request",
        UPDATE_USER_MESSAGE,
    )
    .await;
    assert!(update_sse.contains("memory_commit_completed"));
    for forbidden in [UPDATE_CONTENT, UPDATE_REASON] {
        assert!(
            !update_sse.contains(forbidden),
            "记忆更新 SSE 不得包含正文 `{forbidden}`"
        );
    }
    let updated = repository
        .current(&scope, &created.entry.memory_id)
        .expect("自动 update 后读取不应失败")
        .expect("自动 update 后记忆应存在");
    assert_eq!(updated.current_revision.content, UPDATE_CONTENT);
    assert_eq!(
        updated.current_revision.change_type,
        MemoryChangeType::Update
    );
    assert_eq!(
        updated.current_revision.source.conversation_id(),
        Some(second_conversation_id.as_str())
    );
    provider.set_current(&updated.entry.memory_id, &updated.entry.current_revision_id);

    let correct_sse = run_memory_contract_turn(
        &state,
        &second_conversation_id,
        "memory-contract-correct-request",
        CORRECT_USER_MESSAGE,
    )
    .await;
    assert!(correct_sse.contains("memory_commit_completed"));
    for forbidden in [CORRECT_CONTENT, CORRECT_REASON] {
        assert!(
            !correct_sse.contains(forbidden),
            "记忆纠正 SSE 不得包含正文 `{forbidden}`"
        );
    }
    let corrected = repository
        .current(&scope, &created.entry.memory_id)
        .expect("自动 correct 后读取不应失败")
        .expect("自动 correct 后记忆应存在");
    assert_eq!(corrected.current_revision.content, CORRECT_CONTENT);
    assert_eq!(
        corrected.current_revision.change_type,
        MemoryChangeType::Correct
    );
    let history = repository
        .revision_history(&scope, &created.entry.memory_id, None, 10)
        .expect("自动记忆版本历史应可审计");
    assert_eq!(history.revisions.len(), 3);
    assert!(history.revisions.iter().any(|revision| {
        revision.content == CREATE_CONTENT
            && revision.change_type == MemoryChangeType::Create
            && revision.state == MemoryRevisionState::Superseded
    }));
    assert!(history.revisions.iter().any(|revision| {
        revision.content == UPDATE_CONTENT
            && revision.change_type == MemoryChangeType::Update
            && revision.state == MemoryRevisionState::Corrected
    }));
    assert!(history.revisions.iter().any(|revision| {
        revision.content == CORRECT_CONTENT
            && revision.change_type == MemoryChangeType::Correct
            && revision.state == MemoryRevisionState::Current
    }));

    let first_process_tool_payloads = memory_tool_session_payloads(
        &state,
        &[
            first_conversation_id.clone(),
            second_conversation_id.clone(),
        ],
    )
    .await;
    for forbidden in [
        CREATE_CONTENT,
        UPDATE_CONTENT,
        CORRECT_CONTENT,
        CREATE_REASON,
        UPDATE_REASON,
        CORRECT_REASON,
        "茉莉花茶",
    ] {
        assert!(
            !first_process_tool_payloads.contains(forbidden),
            "记忆 Tool Session 收据不得包含 `{forbidden}`"
        );
    }
    assert!(first_process_tool_payloads.contains("memory_mutate_result"));
    assert!(first_process_tool_payloads.contains("memory_query_result"));

    drop(repository);
    drop(state);

    let mut restarted_state = build_test_state(&data_dir);
    let (restarted_runtime_memory, restarted_management_memory) =
        crate::runtime_support::build_sqlite_memory_services(&data_dir)
            .expect("进程重启后应重新构造生产 SQLite 记忆服务束");
    Arc::get_mut(&mut restarted_state)
        .expect("重启测试状态尚未共享")
        .memory = Some(restarted_runtime_memory);
    drop(restarted_management_memory);
    *restarted_state.provider.lock().await = Some(provider.clone());
    let third_conversation_id = reset_memory_contract_session(&restarted_state).await;
    assert_ne!(third_conversation_id, first_conversation_id);
    assert_ne!(third_conversation_id, second_conversation_id);
    let restarted_query_sse = run_memory_contract_turn(
        &restarted_state,
        &third_conversation_id,
        "memory-contract-query-after-restart-request",
        "重启后请再次回忆我的茉莉花茶偏好",
    )
    .await;
    assert!(!restarted_query_sse.contains(CORRECT_CONTENT));
    let query_results = provider.query_results_for_model();
    assert_eq!(query_results.len(), 2);
    assert!(query_results[1].contains(CORRECT_CONTENT));
    assert!(!query_results[1].contains(UPDATE_CONTENT));

    let restarted_repository =
        SqliteMemoryRepository::open(&data_dir).expect("重启后应打开记忆 Repository");
    let after_restart = restarted_repository
        .current(&scope, &created.entry.memory_id)
        .expect("重启后读取不应失败")
        .expect("重启后当前记忆应存在");
    assert_eq!(after_restart.current_revision.content, CORRECT_CONTENT);
    let restarted_tool_payloads =
        memory_tool_session_payloads(&restarted_state, &[third_conversation_id]).await;
    assert!(!restarted_tool_payloads.contains(CORRECT_CONTENT));
    assert!(!restarted_tool_payloads.contains("茉莉花茶"));
    assert!(restarted_tool_payloads.contains("memory_query_result"));

    drop(restarted_repository);
    drop(restarted_state);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn chat_stream_emits_structured_error_when_chat_provider_is_missing() {
    let config_dir = unique_temp_dir("chat-provider-missing");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    *state.provider.lock().await = None;
    let conversation_id = state
        .runtime_service
        .active_conversation_id()
        .expect("应能读取当前会话 ID");
    let app = api_routes().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "检查未配置模型错误",
                        "conversation_id": conversation_id,
                        "client_request_id": "missing-provider-request"
                    })
                    .to_string(),
                ))
                .expect("应能构造未配置模型的流式请求"),
        )
        .await
        .expect("未配置模型时仍应返回 SSE 响应");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取未配置模型的流式响应");
    let payloads = String::from_utf8(body.to_vec())
        .expect("SSE 响应应为 UTF-8")
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("SSE data 应为 JSON"))
        .collect::<Vec<_>>();

    assert!(payloads.iter().any(|payload| {
        payload["type"] == "error"
            && payload["content"]
                .as_str()
                .is_some_and(|content| content.contains("聊天模型未配置"))
    }));
    assert_eq!(
        payloads.last().and_then(|payload| payload["type"].as_str()),
        Some("done")
    );

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn history_only_observes_private_turn_after_commit() {
    let config_dir = unique_temp_dir("private-turn-history");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    *state.provider.lock().await = Some(Arc::new(GatedChatProvider {
        entered: entered.clone(),
        release: release.clone(),
    }));
    let app = api_routes().with_state(state);

    let stream_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "提交前不可见",
                        "conversation_id": "default",
                        "client_request_id": "private-turn-history"
                    })
                    .to_string(),
                ))
                .expect("应能构造门控聊天请求"),
        )
        .await
        .expect("门控聊天请求应被受理");
    entered
        .acquire()
        .await
        .expect("模型流应进入门控点")
        .forget();

    let during_turn = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/history")
                .body(Body::empty())
                .expect("应能构造回合中历史请求"),
        )
        .await
        .expect("回合中历史查询应成功");
    let during_payload = response_json(during_turn).await;
    assert_eq!(during_payload["messages"], serde_json::json!([]));

    release.add_permits(1);
    let stream_body = to_bytes(stream_response.into_body(), usize::MAX)
        .await
        .expect("释放模型后流式响应应完成");
    assert!(String::from_utf8_lossy(&stream_body).contains("门控测试回复"));

    let after_commit = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/history")
                .body(Body::empty())
                .expect("应能构造提交后历史请求"),
        )
        .await
        .expect("提交后历史查询应成功");
    let committed_payload = response_json(after_commit).await;
    let contents = committed_payload["messages"]
        .as_array()
        .expect("历史应为消息数组")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(contents, ["提交前不可见", "门控测试回复。"]);

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn failed_turn_never_publishes_private_messages_to_history() {
    let config_dir = unique_temp_dir("private-turn-failure");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    *state.provider.lock().await = Some(Arc::new(FailingChatProvider));
    let app = api_routes().with_state(state);

    let failed_stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "失败后不可见",
                        "conversation_id": "default",
                        "client_request_id": "private-turn-failure"
                    })
                    .to_string(),
                ))
                .expect("应能构造失败聊天请求"),
        )
        .await
        .expect("失败聊天请求仍应建立 SSE");
    let body = to_bytes(failed_stream.into_body(), usize::MAX)
        .await
        .expect("失败流应正常结束");
    assert!(String::from_utf8_lossy(&body).contains("注入模型失败"));

    let history = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/history")
                .body(Body::empty())
                .expect("应能构造失败后历史请求"),
        )
        .await
        .expect("失败后历史查询应成功");
    let payload = response_json(history).await;
    assert_eq!(payload["messages"], serde_json::json!([]));

    let sessions = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/sessions")
                .body(Body::empty())
                .expect("应能构造失败后会话列表请求"),
        )
        .await
        .expect("失败后会话列表查询应成功");
    let sessions_payload = response_json(sessions).await;
    assert!(
        sessions_payload["sessions"]
            .as_array()
            .expect("会话列表应为数组")
            .iter()
            .all(|session| session["can_resume"] == false),
        "失败回合的审计事件不得被发布成可恢复会话"
    );

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn chat_stream_get_is_method_not_allowed_without_side_effect() {
    let config_dir = unique_temp_dir("chat-get-method");
    let app = api_routes().with_state(build_test_state(&config_dir));
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/chat/stream?message=%E4%B8%8D%E5%BA%94%E6%89%A7%E8%A1%8C")
                .body(Body::empty())
                .expect("应能构造旧版流式聊天请求"),
        )
        .await
        .expect("路由应能拒绝旧版流式聊天请求");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(header::ALLOW).unwrap(), "POST");
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取 405 响应");
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("405 响应应为 JSON");
    assert_eq!(payload["error"], "流式聊天只接受 POST JSON 请求。");
    assert_eq!(payload["code"], "stream_post_required");
}

#[tokio::test]
async fn runtime_approval_guards_turn_and_makes_identical_retry_idempotent() {
    let config_dir = unique_temp_dir("approval-contract");
    let state = build_test_state(&config_dir);
    let (decision_tx, decision_rx) = oneshot::channel();
    let lease = state
        .runtime_service
        .begin_turn("turn-current", "default")
        .expect("应能开始审批测试回合");
    lease.mark_running().expect("审批测试回合应进入运行态");
    lease
        .wait_for_approval()
        .expect("审批测试回合应进入等待审批态");
    state
        .runtime_service
        .register_pending_approval(
            "approval-contract".to_string(),
            PendingApproval {
                turn_id: "turn-current".to_string(),
                tool_name: "command".to_string(),
                risk: "high".to_string(),
                summary: "执行测试命令".to_string(),
                tx: decision_tx,
            },
        )
        .await
        .expect("应能登记审批测试请求");
    let app = api_routes().with_state(state.clone());

    let stale = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/approvals/approval-contract/approve")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"turn_id":"turn-stale"}"#))
                .expect("应能构造过期回合审批请求"),
        )
        .await
        .expect("路由应拒绝过期回合审批");
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_payload = response_json(stale).await;
    assert_flat_api_error(&stale_payload);
    assert_eq!(stale_payload["code"], "stale_turn");
    assert_eq!(
        state.runtime_service.pending_interaction_counts().await.0,
        1
    );

    let approve = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/approvals/approval-contract/approve")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"turn_id":"turn-current"}"#))
                .expect("应能构造审批请求"),
        )
        .await
        .expect("路由应处理审批请求");
    assert_eq!(approve.status(), StatusCode::OK);
    let approve_payload = response_json(approve).await;
    assert_eq!(approve_payload["approval_id"], "approval-contract");
    assert_eq!(approve_payload["approved"], true);
    let decision = decision_rx.await.expect("等待中的工具应收到审批结果");
    assert!(decision.approved);
    assert!(decision.reason.is_none());

    let identical_retry = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/approvals/approval-contract/approve")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"turn_id":"turn-current"}"#))
                .expect("应能构造相同审批重试"),
        )
        .await
        .expect("相同审批重试应幂等成功");
    assert_eq!(identical_retry.status(), StatusCode::OK);
    let identical_payload = response_json(identical_retry).await;
    assert_eq!(identical_payload, approve_payload);

    let conflicting_retry = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/approvals/approval-contract/reject")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"turn_id":"turn-current"}"#))
                .expect("应能构造冲突审批重试"),
        )
        .await
        .expect("路由应拒绝冲突审批重试");
    assert_eq!(conflicting_retry.status(), StatusCode::CONFLICT);
    let conflict_payload = response_json(conflicting_retry).await;
    assert_flat_api_error(&conflict_payload);
    assert_eq!(conflict_payload["code"], "decision_conflict");
}

#[tokio::test]
async fn runtime_user_question_retries_are_idempotent_but_conflicts_return_409() {
    let config_dir = unique_temp_dir("question-contract");
    let state = build_test_state(&config_dir);
    let (decision_tx, decision_rx) = oneshot::channel();
    let lease = state
        .runtime_service
        .begin_turn("turn-question", "default")
        .expect("应能开始问答测试回合");
    lease.mark_running().expect("问答测试回合应进入运行态");
    lease.wait_for_user().expect("问答测试回合应进入等待态");
    state
        .runtime_service
        .register_pending_user_question(
            "question-contract".to_string(),
            PendingUserQuestion {
                turn_id: "turn-question".to_string(),
                tool_name: "request_user_input".to_string(),
                summary: "选择发布策略".to_string(),
                tx: decision_tx,
            },
        )
        .await
        .expect("应能登记问答测试请求");
    let app = api_routes().with_state(state);
    let answer_body = serde_json::json!({
        "turn_id": "turn-question",
        "answers": { "release": "stable" },
        "annotations": { "release": "优先稳定" }
    })
    .to_string();

    let answer = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/user-questions/question-contract/answer")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(answer_body.clone()))
                .expect("应能构造用户问题回答"),
        )
        .await
        .expect("路由应处理用户问题回答");
    assert_eq!(answer.status(), StatusCode::OK);
    let answer_payload = response_json(answer).await;
    assert_eq!(answer_payload["request_id"], "question-contract");
    assert_eq!(answer_payload["answered"], true);
    let decision = decision_rx.await.expect("等待中的问题应收到回答");
    assert!(decision.answered);
    assert_eq!(
        decision.answers,
        Some(serde_json::json!({ "release": "stable" }))
    );

    let identical_retry = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/user-questions/question-contract/answer")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(answer_body))
                .expect("应能构造相同回答重试"),
        )
        .await
        .expect("相同回答重试应幂等成功");
    assert_eq!(identical_retry.status(), StatusCode::OK);
    let identical_payload = response_json(identical_retry).await;
    assert_eq!(identical_payload, answer_payload);

    let conflicting_retry = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runtime/user-questions/question-contract/answer")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": "turn-question",
                        "answers": { "release": "canary" },
                        "annotations": { "release": "优先速度" }
                    })
                    .to_string(),
                ))
                .expect("应能构造冲突回答重试"),
        )
        .await
        .expect("路由应拒绝冲突回答重试");
    assert_eq!(conflicting_retry.status(), StatusCode::CONFLICT);
    let conflict_payload = response_json(conflicting_retry).await;
    assert_flat_api_error(&conflict_payload);
    assert_eq!(conflict_payload["code"], "decision_conflict");
}

#[tokio::test]
async fn runtime_state_reports_monotonic_revision_and_busy_phase() {
    let config_dir = unique_temp_dir("runtime-state-contract");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let turn = state
        .runtime_service
        .begin_turn("turn-state", "default")
        .expect("空闲运行时应能开始回合");
    turn.mark_running().expect("准备中的回合应能进入运行态");
    let expected_running_revision = state
        .runtime_service
        .snapshot()
        .expect("应能读取运行时快照")
        .state_revision;
    let app = api_routes().with_state(state.clone());

    let running = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/state")
                .body(Body::empty())
                .expect("应能构造运行时状态请求"),
        )
        .await
        .expect("路由应返回运行时状态");
    assert_eq!(running.status(), StatusCode::OK);
    let running_payload = response_json(running).await;
    assert_eq!(running_payload["state_revision"], expected_running_revision);
    assert_eq!(running_payload["busy_turn"]["turn_id"], "turn-state");
    assert_eq!(running_payload["busy_turn"]["phase"], "running");

    turn.wait_for_approval()
        .expect("运行中的回合应能进入等待审批态");
    let waiting = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/runtime/state")
                .body(Body::empty())
                .expect("应能构造第二次运行时状态请求"),
        )
        .await
        .expect("路由应返回更新后的运行时状态");
    assert_eq!(waiting.status(), StatusCode::OK);
    let waiting_payload = response_json(waiting).await;
    assert!(
        waiting_payload["state_revision"].as_u64().unwrap()
            > running_payload["state_revision"].as_u64().unwrap()
    );
    assert_eq!(waiting_payload["busy_turn"]["phase"], "waiting_approval");
}

#[tokio::test]
async fn chat_stream_rejects_stale_conversation_and_duplicate_client_request() {
    let config_dir = unique_temp_dir("chat-request-contract");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }
    let state = build_test_state(&config_dir);
    let app = api_routes().with_state(state.clone());

    let stale_conversation = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "message": "不应创建回合",
                        "conversation_id": "stale-conversation",
                        "client_request_id": "stale-request"
                    })
                    .to_string(),
                ))
                .expect("应能构造过期会话请求"),
        )
        .await
        .expect("路由应拒绝过期会话请求");
    assert_eq!(stale_conversation.status(), StatusCode::CONFLICT);
    let stale_payload = response_json(stale_conversation).await;
    assert_flat_api_error(&stale_payload);
    assert!(state.chat_request_ids.lock().await.is_empty());

    let accepted_body = serde_json::json!({
        "message": "只应创建一次回合",
        "conversation_id": "default",
        "client_request_id": "deduplicated-request"
    })
    .to_string();
    let accepted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(accepted_body.clone()))
                .expect("应能构造首个聊天请求"),
        )
        .await
        .expect("首个聊天请求应被受理");
    assert_eq!(accepted.status(), StatusCode::OK);
    drop(accepted);

    let duplicate = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chat/stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(accepted_body))
                .expect("应能构造重复聊天请求"),
        )
        .await
        .expect("重复聊天请求应被拒绝");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate_payload = response_json(duplicate).await;
    assert_flat_api_error(&duplicate_payload);
    assert!(
        duplicate_payload["message"]
            .as_str()
            .unwrap()
            .contains("不会重复创建回合")
    );
    assert_eq!(state.chat_request_ids.lock().await.len(), 1);
}

#[tokio::test]
async fn runtime_session_fork_persists_inherited_todos() {
    let config_dir = unique_temp_dir("fork-todos");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }

    let source_conversation_id = "session-source";
    let state = build_test_state(&config_dir);
    let repository = state
        .runtime_service
        .session_repository()
        .await
        .expect("应打开测试会话仓储");
    repository
        .ensure_persona_binding(
            source_conversation_id,
            "router-test-persona",
            "测试角色 router-test-persona",
            "1.0.0",
        )
        .await
        .expect("应建立来源会话绑定");
    repository
            .append_event(
                source_conversation_id,
                Some("turn-source".to_string()),
                "user",
                serde_json::json!({"conversation_id": source_conversation_id, "content": "继续这个任务"}),
            )
            .await
            .expect("应写入来源用户事件");
    repository
            .append_event(
                source_conversation_id,
                Some("turn-source".to_string()),
                "todo_state",
                serde_json::json!({
                    "conversation_id": source_conversation_id,
                    "turn_id": "turn-source",
                    "todos": [{ "id": "build", "content": "补齐 todo 展示", "status": "in_progress", "priority": "high" }],
                    "summary": "来源任务清单",
                    "updated_at": "2026-06-24T10:01:00Z"
                }),
            )
            .await
            .expect("应写入来源任务状态");
    repository
        .append_event(
            source_conversation_id,
            Some("turn-source".to_string()),
            "turn_committed",
            serde_json::json!({"conversation_id": source_conversation_id, "outcome": "committed"}),
        )
        .await
        .expect("应写入来源提交终态");

    let app = api_routes().with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/runtime/sessions/{source_conversation_id}/fork"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("应能构造会话分叉请求"),
        )
        .await
        .expect("路由应能处理会话分叉请求");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取分叉响应");
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("分叉响应应为 JSON");
    let forked_conversation_id = payload["conversation_id"]
        .as_str()
        .expect("分叉响应应返回新会话 ID");
    let (session_store, _) = muse_runtime::session::SessionStore::open(&config_dir)
        .await
        .expect("应能打开 v3 会话存储");
    let forked_events = session_store
        .events_for_conversation(forked_conversation_id)
        .await
        .expect("应能读取新会话事件");
    let todo_records = forked_events
        .iter()
        .filter(|record| record.kind == "todo_state")
        .collect::<Vec<_>>();
    let fork_snapshots = forked_events
        .iter()
        .filter(|record| record.kind == "session_fork_snapshot")
        .collect::<Vec<_>>();

    assert_eq!(fork_snapshots.len(), 1);
    assert_eq!(
        fork_snapshots[0].payload["messages"][0]["content"],
        serde_json::json!("继续这个任务")
    );
    assert_eq!(todo_records.len(), 1);
    assert_eq!(
        todo_records[0].payload["conversation_id"],
        serde_json::json!(forked_conversation_id)
    );
    assert_eq!(
        todo_records[0].payload["todos"][0]["id"],
        serde_json::json!("build")
    );
    assert_eq!(
        todo_records[0].payload["source_conversation_id"],
        serde_json::json!(source_conversation_id)
    );

    // 用全新的 AppState 模拟进程重启；恢复必须只依赖分叉会话自身的 v3 事件。
    let restarted_state = build_test_state(&config_dir);
    let resume_response = api_routes()
        .with_state(restarted_state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/runtime/sessions/{forked_conversation_id}/resume"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::empty())
                .expect("应能构造分叉会话恢复请求"),
        )
        .await
        .expect("重启后的路由应能恢复分叉会话");
    assert_eq!(resume_response.status(), StatusCode::OK);

    let history_response = api_routes()
        .with_state(restarted_state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/history?conversation_id={forked_conversation_id}"))
                .body(Body::empty())
                .expect("应能构造分叉会话历史请求"),
        )
        .await
        .expect("重启后的路由应能读取分叉会话历史");
    assert_eq!(history_response.status(), StatusCode::OK);
    let history = response_json(history_response).await;
    assert_eq!(history["messages"][0]["content"], "继续这个任务");
}

#[tokio::test]
async fn persona_asset_upload_accepts_files_above_axum_default_body_limit() {
    let config_dir = unique_temp_dir("asset-upload");
    let _env_guard = ENV_LOCK.lock().await;
    let _data_dir_guard = MuseDataDirEnvGuard {
        previous: std::env::var_os("MUSE_DATA_DIR"),
    };
    unsafe {
        std::env::set_var("MUSE_DATA_DIR", &config_dir);
    }

    let boundary = "muse-upload-boundary";
    let mut image_bytes = vec![0_u8; 3 * 1024 * 1024];
    image_bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"large.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(&image_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let app = api_routes().with_state(build_test_state(&config_dir));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/assets/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .expect("应能构造图片上传请求"),
        )
        .await
        .expect("路由应能处理图片上传请求");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("应能读取上传响应");
    let payload: serde_json::Value = serde_json::from_slice(&body).expect("上传响应应为 JSON");
    assert!(
        payload["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("/api/assets/uploaded/"))
    );
    let uploaded_entries = std::fs::read_dir(config_dir.join("assets").join("uploaded"))
        .expect("应写入上传资源目录")
        .count();
    assert_eq!(uploaded_entries, 1);
}

// ---- Persona 长期记忆管理路由契约 ----

use crate::runtime_support::{
    MemoryManagementAudit, MemoryManagementCommands, MemoryServices,
    clear_memory_services_for_test, install_memory_services, memory_services_test_guard,
};
use muse_core::app::memory_storage::SqliteMemoryRepository;
use muse_core::domain::memory::{
    MemoryCategory, MemoryChangeType, MemoryDeleteConfirmation, MemoryDeleteConfirmationSource,
    MemoryDeleteParams, MemoryImportance, MemoryManagementAuthorization, MemoryManagementBinding,
    MemoryManagementContentParams, MemoryQueryItem, MemoryRevision, MemoryRevisionId,
    MemoryRevisionState, MemorySafetyAssessment, MemorySensitivityRequest, MemorySourceEvidence,
};

/// 测试用敏感策略：两个门阶段一律允许，策略版本固定。
struct AllowAllMemorySensitivity;

impl MemorySensitivityPolicy for AllowAllMemorySensitivity {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        MemorySafetyAssessment::Allowed {
            stage: request.stage,
            policy_version: "test-policy/v1".to_string(),
        }
    }
}

/// 用真实 Repository 实现测试命令端口；稳定身份模拟集成层的持久幂等绑定。
struct RepositoryMemoryCommands {
    repository: Arc<SqliteMemoryRepository>,
    delete_failures_remaining: AtomicU64,
}

impl RepositoryMemoryCommands {
    fn new(repository: Arc<SqliteMemoryRepository>, delete_failures: u64) -> Self {
        Self {
            repository,
            delete_failures_remaining: AtomicU64::new(delete_failures),
        }
    }

    fn binding(
        scope: &MemoryPersonaScope,
        operation_id: &str,
    ) -> Result<MemoryManagementBinding, MemoryError> {
        let stable_time = "2099-01-01T00:00:00+00:00";
        let authorization = MemoryManagementAuthorization::from_runtime(
            scope.clone(),
            format!("memory-action-{operation_id}"),
            stable_time,
        )?;
        MemoryManagementBinding::bind(
            authorization,
            operation_id,
            stable_time,
            stable_time,
            stable_time,
        )
    }
}

impl MemoryManagementCommands for RepositoryMemoryCommands {
    fn create(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        request: &MemoryCreateRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        let mutation = MemoryManagementContentMutation::bind(
            MemoryManagementContentParams::Create {
                category: request.category,
                content: request.content.clone(),
                importance: request.importance,
                event_time: request.event_time.clone(),
                change_reason: request.change_reason.clone(),
            },
            Self::binding(scope, operation_id)?,
            MemoryId(format!("memory-{operation_id}")),
            MemoryRevisionId(format!("memory-revision-{operation_id}")),
            &AllowAllMemorySensitivity,
        )?;
        self.repository
            .apply_management_content_mutation(&mutation, &AllowAllMemorySensitivity)
    }

    fn correct(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryCorrectRequest,
    ) -> Result<MemoryMutationReceipt, MemoryError> {
        let mutation = MemoryManagementContentMutation::bind(
            MemoryManagementContentParams::Correct {
                memory_id: memory_id.clone(),
                expected_revision_id: request.expected_revision_id.clone(),
                category: request.category,
                content: request.content.clone(),
                event_time: request.event_time.clone(),
                change_reason: request.change_reason.clone(),
            },
            Self::binding(scope, operation_id)?,
            memory_id.clone(),
            MemoryRevisionId(format!("memory-revision-{operation_id}")),
            &AllowAllMemorySensitivity,
        )?;
        self.repository
            .apply_management_content_mutation(&mutation, &AllowAllMemorySensitivity)
    }

    fn adjust_importance(
        &self,
        scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
        operation_id: &str,
        request: &MemoryImportanceAdjustRequest,
    ) -> Result<MemoryImportanceAdjustmentReceipt, MemoryError> {
        let adjustment = MemoryImportanceAdjustment::bind(
            Self::binding(scope, operation_id)?,
            memory_id.clone(),
            request.expected_revision_id.clone(),
            request.expected_importance,
            request.importance,
        )?;
        self.repository.adjust_importance(&adjustment)
    }

    fn delete(
        &self,
        scope: &MemoryPersonaScope,
        operation_id: &str,
        params: &MemoryDeleteParams,
    ) -> Result<MemoryDeleteReceipt, MemoryError> {
        if self
            .delete_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_ok()
        {
            return Err(MemoryError::new(
                MemoryErrorCode::DeletionAuthorityUnavailable,
            ));
        }
        let confirmation = MemoryDeleteConfirmation::new(
            operation_id,
            scope,
            params,
            "2026-08-02T00:00:00+00:00",
            "2099-01-01T00:00:00+00:00",
            MemoryDeleteConfirmationSource::PersonaManagement {
                action_id: operation_id.to_string(),
            },
        )?;
        let request =
            ConfirmedMemoryDeleteRequest::bind(params.clone(), scope.clone(), confirmation)?;
        self.repository
            .delete_confirmed(&request, self.repository.deletion_authority())
    }
}

struct StubMemoryRetriever {
    items: Vec<MemoryQueryItem>,
}

impl MemoryRetriever for StubMemoryRetriever {
    fn retrieve(
        &self,
        request: &MemoryRetrievalRequest,
    ) -> Result<MemoryQueryPageReceipt, MemoryError> {
        let items = self
            .items
            .iter()
            .filter(|item| {
                request
                    .filters()
                    .category()
                    .is_none_or(|category| item.category == category)
                    && request
                        .filters()
                        .importance()
                        .is_none_or(|importance| item.importance == importance)
            })
            .cloned()
            .collect();
        Ok(MemoryQueryPageReceipt::new(items, None))
    }
}

struct StubMemoryAudit {
    count: u64,
    revisions: Vec<MemoryRevision>,
}

impl MemoryManagementAudit for StubMemoryAudit {
    fn revision_history(
        &self,
        _scope: &MemoryPersonaScope,
        memory_id: &MemoryId,
    ) -> Result<Vec<MemoryRevision>, MemoryError> {
        if self
            .revisions
            .iter()
            .any(|revision| &revision.memory_id == memory_id)
        {
            Ok(self.revisions.clone())
        } else {
            Err(MemoryError::new(MemoryErrorCode::MemoryNotFound))
        }
    }

    fn active_memory_count(&self, _scope: &MemoryPersonaScope) -> Result<u64, MemoryError> {
        Ok(self.count)
    }
}

fn install_test_memory_services(
    repository: SqliteMemoryRepository,
    retriever_items: Vec<MemoryQueryItem>,
    audit: StubMemoryAudit,
) -> Arc<SqliteMemoryRepository> {
    install_test_memory_services_with_delete_failures(repository, retriever_items, audit, 0).0
}

fn install_empty_test_memory_services(config_dir: &Path) {
    install_test_memory_services(
        open_test_memory_repository(&config_dir.join("memory-test-store")),
        Vec::new(),
        StubMemoryAudit {
            count: 0,
            revisions: Vec::new(),
        },
    );
}

fn install_test_memory_services_with_delete_failures(
    repository: SqliteMemoryRepository,
    retriever_items: Vec<MemoryQueryItem>,
    audit: StubMemoryAudit,
    delete_failures: u64,
) -> (Arc<SqliteMemoryRepository>, Arc<RepositoryMemoryCommands>) {
    let repository = Arc::new(repository);
    let commands = Arc::new(RepositoryMemoryCommands::new(
        Arc::clone(&repository),
        delete_failures,
    ));
    install_memory_services(MemoryServices {
        repository: Arc::clone(&repository),
        retriever: Arc::new(StubMemoryRetriever {
            items: retriever_items,
        }),
        commands: commands.clone(),
        audit: Arc::new(audit),
    });
    (repository, commands)
}

fn open_test_memory_repository(dir: &Path) -> SqliteMemoryRepository {
    SqliteMemoryRepository::open(dir).expect("应能打开测试记忆 Repository")
}

/// 直接经 Repository 端口写入种子记忆，不旁路直写表。
fn seed_test_memory(
    repository: &SqliteMemoryRepository,
    persona_id: &str,
    content: &str,
) -> (MemoryId, MemoryRevisionId) {
    static SEED_SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEED_SEQ.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().to_rfc3339();
    let scope = MemoryPersonaScope::new(persona_id).expect("应能绑定测试 scope");
    let authorization = MemoryManagementAuthorization::from_runtime(
        scope,
        format!("test-action-{seq}"),
        now.clone(),
    )
    .expect("应能创建测试管理授权");
    let binding = MemoryManagementBinding::bind(
        authorization,
        format!("test-op-{seq}"),
        now.clone(),
        now.clone(),
        now,
    )
    .expect("应能创建测试管理绑定");
    let memory_id = MemoryId(format!("memory-seed-{seq}"));
    let revision_id = MemoryRevisionId(format!("memory-seed-{seq}-rev"));
    let mutation = MemoryManagementContentMutation::bind(
        MemoryManagementContentParams::Create {
            category: MemoryCategory::UserFact,
            content: content.to_string(),
            importance: MemoryImportance::Normal,
            event_time: None,
            change_reason: "测试种子记忆".to_string(),
        },
        binding,
        memory_id.clone(),
        revision_id.clone(),
        &AllowAllMemorySensitivity,
    )
    .expect("应能绑定测试管理变更");
    repository
        .apply_management_content_mutation(&mutation, &AllowAllMemorySensitivity)
        .expect("应能写入测试种子记忆");
    (memory_id, revision_id)
}

fn memory_scope(persona_id: &str) -> MemoryPersonaScope {
    MemoryPersonaScope::new(persona_id).expect("应能绑定测试 scope")
}

fn stub_query_item(
    memory_id: &str,
    category: MemoryCategory,
    importance: MemoryImportance,
) -> MemoryQueryItem {
    MemoryQueryItem {
        memory_id: MemoryId(memory_id.to_string()),
        revision_id: MemoryRevisionId(format!("{memory_id}-rev")),
        category,
        facet: MemoryFacet::default_for_category(category),
        keywords: vec!["测试".to_string()],
        content: format!("{memory_id} 的测试内容"),
        importance,
        event_time: None,
        recorded_at: "2026-07-30T00:00:00+00:00".to_string(),
        valid_from: "2026-07-30T00:00:00+00:00".to_string(),
        valid_to: None,
        change_type: MemoryChangeType::Create,
        change_reason: "测试".to_string(),
    }
}

fn stub_audit_revision(memory_id: &MemoryId, state: MemoryRevisionState) -> MemoryRevision {
    MemoryRevision {
        revision_id: MemoryRevisionId(format!("{}-{state:?}-rev", memory_id.0)),
        memory_id: memory_id.clone(),
        facet: MemoryFacet::Other,
        keywords: vec!["测试".to_string()],
        content: "历史测试内容".to_string(),
        event_time: None,
        recorded_at: "2026-07-30T00:00:00+00:00".to_string(),
        valid_from: "2026-07-30T00:00:00+00:00".to_string(),
        valid_to: None,
        change_type: MemoryChangeType::Correct,
        change_reason: "测试纠正".to_string(),
        source: MemorySourceEvidence::PersonaManagement {
            action_id: "test-action-history".to_string(),
            authorized_at: "2026-07-30T00:00:00+00:00".to_string(),
        },
        safety_policy_version: "test-policy/v1".to_string(),
        state,
    }
}

#[tokio::test]
async fn persona_memory_routes_return_stable_503_when_unwired() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-unwired");
    let app = api_routes().with_state(build_test_state(&config_dir));

    let cases: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        (
            "GET",
            "/personas/router-test-persona/memories?query=用户".to_string(),
            None,
        ),
        (
            "GET",
            "/personas/router-test-persona/memories/memory-x".to_string(),
            None,
        ),
        (
            "GET",
            "/personas/router-test-persona/memories/memory-x/history".to_string(),
            None,
        ),
        (
            "POST",
            "/personas/router-test-persona/memories".to_string(),
            Some(serde_json::json!({
                "category": "user_fact",
                "content": "用户喜欢喝绿茶",
                "importance": "normal",
                "change_reason": "用户明确告知"
            })),
        ),
        (
            "POST",
            "/personas/router-test-persona/memories/memory-x/correct".to_string(),
            Some(serde_json::json!({
                "expected_revision_id": "rev-x",
                "category": "user_fact",
                "content": "用户喜欢喝红茶",
                "change_reason": "用户纠正"
            })),
        ),
        (
            "POST",
            "/personas/router-test-persona/memories/memory-x/importance".to_string(),
            Some(serde_json::json!({
                "expected_revision_id": "rev-x",
                "expected_importance": "normal",
                "importance": "high"
            })),
        ),
        (
            "DELETE",
            "/personas/router-test-persona/memories/memory-x".to_string(),
            Some(serde_json::json!({"operation_id": "delete-memory-x"})),
        ),
        (
            "DELETE",
            "/personas/router-test-persona/memories".to_string(),
            Some(serde_json::json!({"operation_id": "clear-persona-x"})),
        ),
    ];
    for (method, uri, body) in cases {
        let builder = Request::builder().method(method).uri(&uri);
        let request = match body {
            Some(payload) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .expect("应能构造记忆管理请求"),
            None => builder.body(Body::empty()).expect("应能构造记忆管理请求"),
        };
        let response = app
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|_| panic!("{method} {uri} 应返回响应"));
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri} 未接线应返回 503"
        );
        let payload = response_json(response).await;
        assert_flat_api_error(&payload);
        assert_eq!(
            payload["code"], "memory_repository_unavailable",
            "{method} {uri} 未接线应返回 memory_repository_unavailable"
        );
    }

    // 未接线时删除影响的 memory_count 为 null，前端保持确认按钮禁用。
    let impact = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/deletion-impact")
                .body(Body::empty())
                .expect("应能构造删除影响查询"),
        )
        .await
        .expect("删除影响查询应返回响应");
    assert_eq!(impact.status(), StatusCode::OK);
    let impact_payload = response_json(impact).await;
    assert!(impact_payload["memory_count"].is_null());

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn persona_memory_strict_dto_rejections_use_flat_stable_errors() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-strict-dto");
    let app = api_routes().with_state(build_test_state(&config_dir));
    let cases = [
        (
            "POST",
            "/personas/router-test-persona/memories",
            "{".to_string(),
        ),
        (
            "POST",
            "/personas/router-test-persona/memories",
            serde_json::json!({
                "category": "user_fact",
                "content": "用户喜欢绿茶",
                "importance": "normal",
                "change_reason": "用户明确告知",
                "unexpected": true
            })
            .to_string(),
        ),
        (
            "DELETE",
            "/personas/router-test-persona/memories",
            serde_json::json!({
                "operation_id": "strict-delete",
                "unexpected": true
            })
            .to_string(),
        ),
    ];
    for (method, uri, body) in cases {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("应能构造严格 DTO 失败请求"),
            )
            .await
            .expect("严格 DTO 失败应返回响应");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = response_json(response).await;
        assert_flat_api_error(&payload);
        assert_eq!(payload["code"], "memory_invalid_request");
    }

    let query = app
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?query=用户&unexpected=true")
                .body(Body::empty())
                .expect("应能构造未知查询字段请求"),
        )
        .await
        .expect("未知查询字段应返回响应");
    assert_eq!(query.status(), StatusCode::BAD_REQUEST);
    let payload = response_json(query).await;
    assert_flat_api_error(&payload);
    assert_eq!(payload["code"], "memory_invalid_request");

    let _ = std::fs::remove_dir_all(config_dir);
}

#[tokio::test]
async fn persona_memory_management_roundtrip_and_cross_persona_denial() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-roundtrip");
    let memory_dir = unique_temp_dir("memory-roundtrip-store");
    let repository = install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 0,
            revisions: Vec::new(),
        },
    );
    let state = build_test_state(&config_dir);
    {
        let mut personas = state.personas.lock().await;
        personas
            .create(test_persona("persona-b"))
            .expect("应能创建第二个测试角色");
        personas.save().expect("应能保存第二个测试角色");
    }
    let app = api_routes().with_state(state);

    // 手工新增：201 + durable 收据；scope 只能来自路径。
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/personas/router-test-persona/memories")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "category": "user_preference",
                        "content": "用户喜欢在晚上喝绿茶",
                        "importance": "normal",
                        "change_reason": "用户明确告知偏好",
                        "operation_id": "client-op-create-1"
                    })
                    .to_string(),
                ))
                .expect("应能构造手工新增请求"),
        )
        .await
        .expect("手工新增应返回响应");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_payload = response_json(created).await;
    assert_eq!(created_payload["operation"], "create");
    assert_eq!(created_payload["state"], "durable");
    let memory_id = created_payload["memory_id"]
        .as_str()
        .expect("新增收据应带 memory_id")
        .to_string();
    let revision_id = created_payload["revision_id"]
        .as_str()
        .expect("新增收据应带 revision_id")
        .to_string();

    // 相同 operation_id 的重放返回同一 durable 收据，绝不产生重复记忆。
    let replayed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/personas/router-test-persona/memories")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "category": "user_preference",
                        "content": "用户喜欢在晚上喝绿茶",
                        "importance": "normal",
                        "change_reason": "用户明确告知偏好",
                        "operation_id": "client-op-create-1"
                    })
                    .to_string(),
                ))
                .expect("应能构造重放新增请求"),
        )
        .await
        .expect("重放新增应返回响应");
    assert_eq!(replayed.status(), StatusCode::CREATED);
    let replay_payload = response_json(replayed).await;
    assert_eq!(replay_payload, created_payload);

    let conflicting_replay = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/personas/router-test-persona/memories")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "category": "user_preference",
                        "content": "同一 operation 伪造了不同内容",
                        "importance": "normal",
                        "change_reason": "冲突重放",
                        "operation_id": "client-op-create-1"
                    })
                    .to_string(),
                ))
                .expect("应能构造冲突重放请求"),
        )
        .await
        .expect("冲突重放应返回响应");
    assert_eq!(conflicting_replay.status(), StatusCode::BAD_REQUEST);
    let conflicting_payload = response_json(conflicting_replay).await;
    assert_flat_api_error(&conflicting_payload);
    assert_eq!(conflicting_payload["code"], "memory_invalid_request");

    // 详情：管理来源没有对话跳转索引。
    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}"
                ))
                .body(Body::empty())
                .expect("应能构造记忆详情请求"),
        )
        .await
        .expect("记忆详情应返回响应");
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_payload = response_json(detail).await;
    assert_eq!(detail_payload["entry"]["category"], "user_preference");
    assert_eq!(
        detail_payload["current_revision"]["content"],
        "用户喜欢在晚上喝绿茶"
    );
    assert!(detail_payload["source_conversation_id"].is_null());
    assert!(detail_payload["source_turn_id"].is_null());
    assert!(
        detail_payload["current_revision"].get("source").is_none(),
        "详情不得暴露领域来源证据中的管理操作元数据"
    );

    // 跨 Persona 访问同一 memory_id 一律 404，scope 不随客户端切换。
    let cross = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/personas/persona-b/memories/{memory_id}"))
                .body(Body::empty())
                .expect("应能构造跨角色访问请求"),
        )
        .await
        .expect("跨角色访问应返回响应");
    assert_eq!(cross.status(), StatusCode::NOT_FOUND);
    let cross_payload = response_json(cross).await;
    assert_flat_api_error(&cross_payload);
    assert_eq!(cross_payload["code"], "memory_not_found");

    // 陈旧 revision 的纠正返回 409 memory_revision_conflict。
    let stale_correct = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}/correct"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "expected_revision_id": "rev-stale",
                        "category": "user_preference",
                        "content": "用户喜欢在晚上喝红茶",
                        "change_reason": "用户纠正偏好"
                    })
                    .to_string(),
                ))
                .expect("应能构造陈旧纠正请求"),
        )
        .await
        .expect("陈旧纠正应返回响应");
    assert_eq!(stale_correct.status(), StatusCode::CONFLICT);
    let stale_payload = response_json(stale_correct).await;
    assert_flat_api_error(&stale_payload);
    assert_eq!(stale_payload["code"], "memory_revision_conflict");

    // 正确 revision 的纠正成功；重放同一旧 revision 再次 409，不会重复生效。
    let corrected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}/correct"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "expected_revision_id": revision_id,
                        "category": "user_preference",
                        "content": "用户喜欢在晚上喝红茶",
                        "change_reason": "用户纠正偏好"
                    })
                    .to_string(),
                ))
                .expect("应能构造纠正请求"),
        )
        .await
        .expect("纠正应返回响应");
    assert_eq!(corrected.status(), StatusCode::OK);
    let corrected_payload = response_json(corrected).await;
    assert_eq!(corrected_payload["operation"], "correct");
    assert_eq!(corrected_payload["state"], "durable");
    let corrected_revision_id = corrected_payload["revision_id"]
        .as_str()
        .expect("纠正收据应带新 revision_id")
        .to_string();
    assert_ne!(corrected_revision_id, revision_id);

    // 重要程度：相同目标值被领域拒绝（409），不同目标值成功且不产生新 revision。
    let same_importance = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}/importance"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "expected_revision_id": corrected_revision_id,
                        "expected_importance": "normal",
                        "importance": "normal"
                    })
                    .to_string(),
                ))
                .expect("应能构造相同重要程度请求"),
        )
        .await
        .expect("相同重要程度应返回响应");
    assert_eq!(same_importance.status(), StatusCode::CONFLICT);
    let same_payload = response_json(same_importance).await;
    assert_flat_api_error(&same_payload);
    assert_eq!(same_payload["code"], "memory_invalid_state_transition");

    let adjusted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}/importance"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "expected_revision_id": corrected_revision_id,
                        "expected_importance": "normal",
                        "importance": "high"
                    })
                    .to_string(),
                ))
                .expect("应能构造重要程度调整请求"),
        )
        .await
        .expect("重要程度调整应返回响应");
    assert_eq!(adjusted.status(), StatusCode::OK);
    let adjusted_payload = response_json(adjusted).await;
    assert_eq!(adjusted_payload["previous_importance"], "normal");
    assert_eq!(adjusted_payload["importance"], "high");
    assert!(
        adjusted_payload.get("revision_id").is_none(),
        "重要程度调整不得创建内容 revision"
    );

    // 删除单条：durable 后详情 404。
    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/personas/router-test-persona/memories/{memory_id}"
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"operation_id": "delete-memory-roundtrip"}).to_string(),
                ))
                .expect("应能构造单条删除请求"),
        )
        .await
        .expect("单条删除应返回响应");
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted_payload = response_json(deleted).await;
    assert_eq!(deleted_payload["deleted_memory_count"], 1);
    assert!(
        repository
            .current(
                &memory_scope("router-test-persona"),
                &MemoryId(memory_id.clone())
            )
            .expect("删除后读取不应失败")
            .is_none(),
        "删除后记忆不得再可读"
    );

    // Persona 全清后使用同一 operation 重放，必须返回首次收据且不删除中间新建记忆。
    seed_test_memory(
        &repository,
        "router-test-persona",
        "首次全清应删除的测试记忆",
    );
    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona/memories")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"operation_id": "clear-memory-roundtrip"}).to_string(),
                ))
                .expect("应能构造全清请求"),
        )
        .await
        .expect("全清应返回响应");
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared_payload = response_json(cleared).await;
    assert_eq!(cleared_payload["deleted_memory_count"], 1);

    let (created_between_retries, _) = seed_test_memory(
        &repository,
        "router-test-persona",
        "首次全清成功后新创建的记忆",
    );
    let replayed_clear = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona/memories")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"operation_id": "clear-memory-roundtrip"}).to_string(),
                ))
                .expect("应能构造全清重放请求"),
        )
        .await
        .expect("全清重放应返回响应");
    assert_eq!(replayed_clear.status(), StatusCode::OK);
    assert_eq!(response_json(replayed_clear).await, cleared_payload);
    assert!(
        repository
            .current(
                &memory_scope("router-test-persona"),
                &created_between_retries,
            )
            .expect("重放后读取中间新建记忆不应失败")
            .is_some(),
        "同一 PersonaAll operation 重放不得重新选取首次成功后新建的记忆"
    );

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn persona_memory_list_filters_page_and_browse_mode() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-list");
    let memory_dir = unique_temp_dir("memory-list-store");
    install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        vec![
            stub_query_item("memory-a", MemoryCategory::UserFact, MemoryImportance::High),
            stub_query_item(
                "memory-b",
                MemoryCategory::UserPreference,
                MemoryImportance::Low,
            ),
        ],
        StubMemoryAudit {
            count: 2,
            revisions: Vec::new(),
        },
    );
    let app = api_routes().with_state(build_test_state(&config_dir));

    // 空白 query 进入管理页浏览模式：无关键词时按当前记忆全量分页。
    let blank = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?query=%20")
                .body(Body::empty())
                .expect("应能构造空白检索请求"),
        )
        .await
        .expect("空白检索应返回响应");
    assert_eq!(blank.status(), StatusCode::OK);
    let blank_payload = response_json(blank).await;
    assert_eq!(
        blank_payload["items"]
            .as_array()
            .expect("浏览响应应带 items")
            .len(),
        0,
        "未种子记忆时浏览模式应返回空列表"
    );
    assert_eq!(blank_payload["has_more"], false);

    // category 与 importance 必须绑定到 Retriever，在分页前完成过滤。
    let filtered = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?query=用户&category=user_preference")
                .body(Body::empty())
                .expect("应能构造过滤检索请求"),
        )
        .await
        .expect("过滤检索应返回响应");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered_payload = response_json(filtered).await;
    let items = filtered_payload["items"]
        .as_array()
        .expect("检索响应应带 items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["memory_id"], "memory-b");

    let filtered = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?query=用户&importance=high")
                .body(Body::empty())
                .expect("应能构造重要程度过滤请求"),
        )
        .await
        .expect("重要程度过滤应返回响应");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered_payload = response_json(filtered).await;
    let items = filtered_payload["items"]
        .as_array()
        .expect("检索响应应带 items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["memory_id"], "memory-a");
    assert_eq!(filtered_payload["has_more"], false);

    // 非法游标返回稳定 memory_cursor_invalid。
    let bad_cursor = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?query=用户&cursor=%E4%B8%AD%E6%96%87")
                .body(Body::empty())
                .expect("应能构造非法游标请求"),
        )
        .await
        .expect("非法游标应返回响应");
    assert_eq!(bad_cursor.status(), StatusCode::BAD_REQUEST);
    let cursor_payload = response_json(bad_cursor).await;
    assert_flat_api_error(&cursor_payload);
    assert_eq!(cursor_payload["code"], "memory_cursor_invalid");

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn persona_memory_browse_lists_paginates_and_rejects_tampered_cursor() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-browse");
    let memory_dir = unique_temp_dir("memory-browse-store");
    let repository = open_test_memory_repository(&memory_dir);
    // 浏览器模式首屏：无关键词返回第一页当前记忆，并给出可续页的签名游标。
    for index in 0..25 {
        seed_test_memory(
            &repository,
            "router-test-persona",
            &format!("浏览测试记忆 {index}"),
        );
    }
    install_test_memory_services(
        repository,
        Vec::new(),
        StubMemoryAudit {
            count: 0,
            revisions: Vec::new(),
        },
    );
    let app = api_routes().with_state(build_test_state(&config_dir));
    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories")
                .body(Body::empty())
                .expect("应能构造浏览首屏请求"),
        )
        .await
        .expect("浏览首屏应返回响应");
    assert_eq!(first.status(), StatusCode::OK);
    let first_payload = response_json(first).await;
    let first_items = first_payload["items"]
        .as_array()
        .expect("浏览响应应带 items");
    assert_eq!(first_items.len(), 20, "管理页单页大小应为 20");
    assert_eq!(first_payload["has_more"], true);
    let next_cursor = first_payload["next_cursor"]
        .as_str()
        .expect("有下一页时应返回不透明游标")
        .to_string();

    // 原样回传游标可继续取第二页，末尾页不再返回游标。
    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/personas/router-test-persona/memories?cursor={}",
                    next_cursor
                ))
                .body(Body::empty())
                .expect("应能构造续页请求"),
        )
        .await
        .expect("续页应返回响应");
    assert_eq!(second.status(), StatusCode::OK);
    let second_payload = response_json(second).await;
    assert_eq!(
        second_payload["items"]
            .as_array()
            .expect("续页应带 items")
            .len(),
        5,
        "25 条种子记忆第二页应剩 5 条"
    );
    assert_eq!(second_payload["has_more"], false);
    assert!(second_payload["next_cursor"].is_null());

    // 篡改游标与跨 Persona 复用游标都必须稳定拒绝。
    let tampered = format!("{}x", &next_cursor[..next_cursor.len() - 1]);
    let tampered_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/personas/router-test-persona/memories?cursor={}",
                    tampered
                ))
                .body(Body::empty())
                .expect("应能构造篡改游标请求"),
        )
        .await
        .expect("篡改游标应返回响应");
    assert_eq!(tampered_response.status(), StatusCode::BAD_REQUEST);
    let tampered_payload = response_json(tampered_response).await;
    assert_flat_api_error(&tampered_payload);
    assert_eq!(tampered_payload["code"], "memory_cursor_invalid");

    // 先创建第二个角色（不切换活动角色），复用首屏游标必须被角色绑定拒绝。
    let create_other = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/personas")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "persona": test_persona("another-persona"),
                        "visual_pack_patch": null,
                        "activate_after_create": false
                    })
                    .to_string(),
                ))
                .expect("应能构造创建第二角色请求"),
        )
        .await
        .expect("创建第二角色应返回响应");
    assert_eq!(create_other.status(), StatusCode::CREATED);
    let cross_persona = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/personas/another-persona/memories?cursor={}",
                    next_cursor
                ))
                .body(Body::empty())
                .expect("应能构造跨 Persona 游标请求"),
        )
        .await
        .expect("跨 Persona 游标应返回响应");
    assert_eq!(cross_persona.status(), StatusCode::BAD_REQUEST);
    let cross_payload = response_json(cross_persona).await;
    assert_flat_api_error(&cross_payload);
    assert_eq!(cross_payload["code"], "memory_cursor_invalid");

    // 浏览模式仍按类别过滤当前记忆。
    let preference_filter = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories?category=user_preference")
                .body(Body::empty())
                .expect("应能构造浏览类别过滤请求"),
        )
        .await
        .expect("浏览类别过滤应返回响应");
    assert_eq!(preference_filter.status(), StatusCode::OK);
    let preference_payload = response_json(preference_filter).await;
    assert_eq!(
        preference_payload["items"]
            .as_array()
            .expect("浏览过滤应带 items")
            .len(),
        0,
        "没有 user_preference 种子时浏览过滤应为空"
    );

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn persona_memory_history_and_deletion_impact_use_audit_port() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-history");
    let memory_dir = unique_temp_dir("memory-history-store");
    let known_memory_id = MemoryId("memory-known".to_string());
    install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 3,
            revisions: vec![
                stub_audit_revision(&known_memory_id, MemoryRevisionState::Corrected),
                stub_audit_revision(&known_memory_id, MemoryRevisionState::Current),
            ],
        },
    );
    let app = api_routes().with_state(build_test_state(&config_dir));

    // corrected 审计条目只允许管理历史入口读取。
    let history = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories/memory-known/history")
                .body(Body::empty())
                .expect("应能构造历史查询请求"),
        )
        .await
        .expect("历史查询应返回响应");
    assert_eq!(history.status(), StatusCode::OK);
    let history_payload = response_json(history).await;
    let revisions = history_payload["revisions"]
        .as_array()
        .expect("历史响应应带 revisions");
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0]["state"], "corrected");
    assert!(
        revisions
            .iter()
            .all(|revision| revision.get("source").is_none()),
        "历史入口只能返回 conversation_id/turn_id 来源索引，不得暴露领域 source"
    );
    assert!(
        revisions
            .iter()
            .all(|revision| revision["source_conversation_id"].is_null()
                && revision["source_turn_id"].is_null()),
        "Persona 管理来源不应伪造对话跳转索引"
    );

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/memories/memory-missing/history")
                .body(Body::empty())
                .expect("应能构造未知记忆历史请求"),
        )
        .await
        .expect("未知记忆历史应返回响应");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_payload = response_json(missing).await;
    assert_flat_api_error(&missing_payload);
    assert_eq!(missing_payload["code"], "memory_not_found");

    // 已接线时删除影响返回真实记忆条数。
    let impact = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona/deletion-impact")
                .body(Body::empty())
                .expect("应能构造删除影响查询"),
        )
        .await
        .expect("删除影响查询应返回响应");
    assert_eq!(impact.status(), StatusCode::OK);
    let impact_payload = response_json(impact).await;
    assert_eq!(impact_payload["memory_count"], 3);

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn delete_persona_removes_memories_after_commit_point() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-persona-delete");
    let memory_dir = unique_temp_dir("memory-persona-delete-store");
    let repository = install_test_memory_services(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 1,
            revisions: Vec::new(),
        },
    );
    let (memory_id, _) = seed_test_memory(&repository, "router-test-persona", "用户喜欢喝绿茶");
    let app = api_routes().with_state(build_test_state(&config_dir));

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造角色删除请求"),
        )
        .await
        .expect("角色删除应返回响应");
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(
        repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("角色删除后读取不应失败")
            .is_none(),
        "角色删除必须同时清除该 Persona 的记忆"
    );

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[tokio::test]
async fn delete_persona_memory_failure_keeps_recovery_and_converges_after_restart() {
    let _guard = memory_services_test_guard().await;
    let config_dir = unique_temp_dir("memory-persona-delete-fail");
    let memory_dir = unique_temp_dir("memory-persona-delete-fail-store");
    let (repository, _) = install_test_memory_services_with_delete_failures(
        open_test_memory_repository(&memory_dir),
        Vec::new(),
        StubMemoryAudit {
            count: 1,
            revisions: Vec::new(),
        },
        1,
    );
    let (memory_id, _) = seed_test_memory(&repository, "router-test-persona", "用户喜欢喝绿茶");

    let state = build_test_state(&config_dir);
    let app = api_routes().with_state(state.clone());
    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造角色删除请求"),
        )
        .await
        .expect("角色删除应返回响应");
    assert_eq!(
        deleted.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "提交后的记忆清理失败应返回稳定可重试错误"
    );
    let payload = response_json(deleted).await;
    assert_flat_api_error(&payload);
    assert_eq!(payload["code"], "memory_deletion_authority_unavailable");

    // personas.json 已是提交点；失败后不得回滚角色并形成“角色仍在但记忆已丢失”。
    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/personas/router-test-persona")
                .body(Body::empty())
                .expect("应能构造角色详情请求"),
        )
        .await
        .expect("角色详情应返回响应");
    assert_eq!(
        detail.status(),
        StatusCode::NOT_FOUND,
        "记忆清理失败时角色删除提交点必须保持"
    );
    assert!(
        repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("失败后读取记忆不应失败")
            .is_some(),
        "首次记忆清理失败时明文必须保持，等待恢复记录重试"
    );
    let recovery_path = config_dir.join("runtime/persona-deletion-recovery.json");
    let pending_after_failure: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&recovery_path).expect("记忆清理失败后必须保留 pending 恢复证据"),
    )
    .expect("记忆清理失败后的 pending 恢复证据应可解析");
    assert_eq!(
        pending_after_failure["pending"]["persona_id"],
        "router-test-persona"
    );
    assert!(
        pending_after_failure["pending"]["memory_delete_operation_id"]
            .as_str()
            .is_some_and(|operation_id| !operation_id.is_empty()),
        "记忆清理失败后必须保留原 operation 身份"
    );

    crate::runtime_support::initialize_active_persona_session(&state)
        .await
        .expect("重启恢复应重试 Persona 记忆清理");
    assert!(
        repository
            .current(&memory_scope("router-test-persona"), &memory_id)
            .expect("恢复后读取记忆不应失败")
            .is_none(),
        "恢复记录应使用同一 operation 身份幂等收敛记忆清理"
    );
    let recovery_after_retry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&recovery_path).expect("恢复完成后应保留可解析的空恢复文件"),
    )
    .expect("恢复完成后的恢复文件应可解析");
    assert!(
        recovery_after_retry["pending"].is_null(),
        "只有记忆清理成功后才允许删除 pending 恢复证据"
    );

    clear_memory_services_for_test();
    let _ = std::fs::remove_dir_all(config_dir);
    let _ = std::fs::remove_dir_all(memory_dir);
}

#[test]
fn local_api_docs_cover_memory_routes_and_stable_codes() {
    // docs/local-api.md 是路由与错误码的公开契约，必须与本切片实现保持一致。
    let docs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/local-api.md"
    ))
    .expect("应能读取本地 API 文档");
    for route in [
        "GET /api/personas/{id}/memories",
        "GET /api/personas/{id}/memories/{memory_id}",
        "GET /api/personas/{id}/memories/{memory_id}/history",
        "POST /api/personas/{id}/memories",
        "POST /api/personas/{id}/memories/{memory_id}/correct",
        "POST /api/personas/{id}/memories/{memory_id}/importance",
        "DELETE /api/personas/{id}/memories/{memory_id}",
        "DELETE /api/personas/{id}/memories",
    ] {
        assert!(docs.contains(route), "文档缺少记忆路由 `{route}`");
    }
    for code in [
        "memory_invalid_request",
        "memory_invalid_state_transition",
        "memory_not_found",
        "memory_revision_conflict",
        "memory_persona_scope_mismatch",
        "memory_source_ineligible",
        "memory_sensitive_content_rejected",
        "memory_sensitivity_unavailable",
        "memory_cursor_invalid",
        "memory_cursor_expired",
        "memory_query_rejected",
        "memory_query_budget_exceeded",
        "memory_delete_confirmation_required",
        "memory_deletion_authority_unavailable",
        "memory_deletion_incomplete",
        "memory_repository_unavailable",
    ] {
        assert!(docs.contains(code), "文档缺少记忆稳定码 `{code}`");
    }
    assert!(
        docs.contains("memory_count"),
        "文档缺少删除影响 memory_count 字段"
    );
}
