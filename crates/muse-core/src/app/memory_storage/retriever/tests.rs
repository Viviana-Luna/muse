//! Retriever 行为测试与固定中英文语料评估。
//!
//! 评估测试 `fixed_bilingual_corpus_evaluation` 的输出（`EVAL|` 前缀）是
//! 默认/最大页大小、单页 Token 与 Turn 查询预算建议值的实测证据。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use chrono::Utc;

use super::*;
use crate::domain::memory::{
    MemoryCommitEnvelope, MemoryEntry, MemoryEntryState, MemoryMutateParams, MemoryRepository,
    MemoryRevisionState, MemoryRuntimeBinding, MemorySafetyAssessment, MemorySensitivityPolicy,
    MemorySensitivityRequest, MemorySourceKind, MemoryStagedMutation,
};

const T1: &str = "2026-07-30T01:00:00Z";
const T2: &str = "2026-07-30T02:00:00Z";
const T3: &str = "2026-07-30T03:00:00Z";
const COMMIT_AT: &str = "2026-07-30T10:00:00Z";
const FRESH: &str = "2026-07-30T00:00:00Z";
const AGE_3D: &str = "2026-07-27T00:00:00Z";
const AGE_7D: &str = "2026-07-23T00:00:00Z";
const AGE_14D: &str = "2026-07-16T00:00:00Z";
const AGE_30D: &str = "2026-06-30T00:00:00Z";
const AGE_60D: &str = "2026-05-31T00:00:00Z";
const AGE_90D: &str = "2026-05-01T00:00:00Z";

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "muse-retriever-{label}-{}-{sequence}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct AllowPolicy;

impl MemorySensitivityPolicy for AllowPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        MemorySafetyAssessment::Allowed {
            stage: request.stage,
            policy_version: "测试策略-v1".to_string(),
        }
    }
}

fn scope(persona_id: &str) -> MemoryPersonaScope {
    MemoryPersonaScope::new(persona_id).expect("Persona scope 应有效")
}

fn unique(prefix: &str) -> String {
    let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{sequence}")
}

fn open_repository(directory: &TestDirectory) -> SqliteMemoryRepository {
    SqliteMemoryRepository::open(directory.path()).expect("Repository 应打开成功")
}

#[allow(clippy::too_many_arguments)]
fn stage_create(
    scope: &MemoryPersonaScope,
    conversation_id: &str,
    turn_id: &str,
    operation_id: &str,
    memory_id: &str,
    revision_id: &str,
    category: MemoryCategory,
    content: &str,
    importance: MemoryImportance,
    time: &str,
) -> MemoryStagedMutation {
    let binding = MemoryRuntimeBinding::new(
        scope.clone(),
        conversation_id,
        turn_id,
        operation_id,
        MemorySourceKind::DirectUserMessage,
        time,
        time,
        time,
    )
    .expect("runtime binding 应有效");
    MemoryStagedMutation::stage(
        MemoryMutateParams::Create {
            category,
            content: content.to_string(),
            importance,
            event_time: None,
            change_reason: "测试创建".to_string(),
        },
        binding,
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        &AllowPolicy,
    )
    .expect("第一敏感门应允许测试数据")
}

#[allow(clippy::too_many_arguments)]
fn stage_change(
    scope: &MemoryPersonaScope,
    conversation_id: &str,
    turn_id: &str,
    operation_id: &str,
    memory_id: &str,
    expected_revision_id: &str,
    revision_id: &str,
    content: &str,
    correct: bool,
    time: &str,
) -> MemoryStagedMutation {
    let binding = MemoryRuntimeBinding::new(
        scope.clone(),
        conversation_id,
        turn_id,
        operation_id,
        MemorySourceKind::UserConfirmation,
        time,
        time,
        time,
    )
    .expect("runtime binding 应有效");
    let common = (
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(expected_revision_id.to_string()),
        MemoryCategory::UserPreference,
        content.to_string(),
        MemoryImportance::Normal,
        None,
        "测试变更".to_string(),
    );
    let params = if correct {
        MemoryMutateParams::Correct {
            memory_id: common.0,
            expected_revision_id: common.1,
            category: common.2,
            content: common.3,
            importance: common.4,
            event_time: common.5,
            change_reason: common.6,
        }
    } else {
        MemoryMutateParams::Update {
            memory_id: common.0,
            expected_revision_id: common.1,
            category: common.2,
            content: common.3,
            importance: common.4,
            event_time: common.5,
            change_reason: common.6,
        }
    };
    MemoryStagedMutation::stage(
        params,
        binding,
        MemoryId(memory_id.to_string()),
        MemoryRevisionId(revision_id.to_string()),
        &AllowPolicy,
    )
    .expect("第一敏感门应允许测试数据")
}

fn commit_batch(
    repository: &SqliteMemoryRepository,
    scope: &MemoryPersonaScope,
    conversation_id: &str,
    turn_id: &str,
    mutations: Vec<MemoryStagedMutation>,
) {
    let envelope = MemoryCommitEnvelope::new(
        unique("key"),
        scope.clone(),
        conversation_id,
        turn_id,
        COMMIT_AT,
        mutations,
    )
    .expect("commit envelope 应有效");
    repository
        .apply_committed_batch(&envelope, &AllowPolicy)
        .expect("提交应成功");
}

#[allow(clippy::too_many_arguments)]
fn create_memory(
    repository: &SqliteMemoryRepository,
    scope: &MemoryPersonaScope,
    memory_id: &str,
    revision_id: &str,
    category: MemoryCategory,
    content: &str,
    importance: MemoryImportance,
    time: &str,
) {
    let turn_id = unique("turn");
    commit_batch(
        repository,
        scope,
        "conv",
        &turn_id,
        vec![stage_create(
            scope,
            "conv",
            &turn_id,
            &unique("op"),
            memory_id,
            revision_id,
            category,
            content,
            importance,
            time,
        )],
    );
}

#[allow(clippy::too_many_arguments)]
fn update_memory(
    repository: &SqliteMemoryRepository,
    scope: &MemoryPersonaScope,
    memory_id: &str,
    expected_revision_id: &str,
    revision_id: &str,
    content: &str,
    correct: bool,
    time: &str,
) {
    let turn_id = unique("turn");
    commit_batch(
        repository,
        scope,
        "conv",
        &turn_id,
        vec![stage_change(
            scope,
            "conv",
            &turn_id,
            &unique("op"),
            memory_id,
            expected_revision_id,
            revision_id,
            content,
            correct,
            time,
        )],
    );
}

#[allow(clippy::too_many_arguments)]
fn request(
    scope: &MemoryPersonaScope,
    query: &str,
    limit: Option<u32>,
    cursor: Option<MemoryCursor>,
    as_of: Option<&str>,
    memory_id: Option<&str>,
    include_history: bool,
) -> MemoryRetrievalRequest {
    let params = MemoryQueryParams {
        query: query.to_string(),
        limit,
        cursor,
        as_of: as_of.map(str::to_string),
        memory_id: memory_id.map(|value| MemoryId(value.to_string())),
        include_history,
    };
    MemoryRetrievalRequest::bind(params, scope.clone()).expect("检索请求应有效")
}

fn retrieve_ids(
    retriever: &SqliteMemoryRetriever,
    scope: &MemoryPersonaScope,
    query: &str,
) -> Vec<String> {
    retriever
        .retrieve(&request(scope, query, None, None, None, None, false))
        .expect("查询应成功")
        .items
        .iter()
        .map(|item| item.memory_id.0.clone())
        .collect()
}

fn error_code(result: Result<MemoryQueryPageReceipt, MemoryError>) -> MemoryErrorCode {
    result.expect_err("应返回稳定错误").code()
}

/// 固定中文语料：覆盖中文、混合语言、标点、emoji、同音干扰、散乱凑字与无关干扰项。
/// 任何语料调整都会改变评估输出，必须与评估报告同步更新。
fn seed_eval_corpus(repository: &SqliteMemoryRepository, scope: &MemoryPersonaScope) {
    let corpus = [
        (
            "m-eval-01",
            "用户早餐喜欢吃豆浆油条，通常七点前吃完",
            MemoryImportance::High,
            FRESH,
            MemoryCategory::UserPreference,
        ),
        (
            "m-eval-02",
            "用户早餐习惯在冬天喝热粥，夏天换成酸奶麦片",
            MemoryImportance::Normal,
            FRESH,
            MemoryCategory::UserPreference,
        ),
        (
            "m-eval-03",
            "用户早餐偶尔也会吃面包配咖啡",
            MemoryImportance::Normal,
            AGE_60D,
            MemoryCategory::UserPreference,
        ),
        (
            "m-eval-04",
            "用户回忆小时候早餐总是妈妈做的鸡蛋面",
            MemoryImportance::Low,
            AGE_90D,
            MemoryCategory::SharedExperience,
        ),
        (
            "m-eval-05",
            "用户早上爬山到山上吃早晨带的压缩饼干，早餐留到中午才吃",
            MemoryImportance::Normal,
            FRESH,
            MemoryCategory::SharedExperience,
        ),
        (
            "m-eval-06",
            "上次剧情里角色和用户在山顶看日出 🌄✨，约定下次再去看",
            MemoryImportance::Low,
            AGE_7D,
            MemoryCategory::StoryState,
        ),
        (
            "m-eval-07",
            "用户正在学习 Rust 的所有权与生命周期，每天晚饭后练习一小时",
            MemoryImportance::Normal,
            AGE_3D,
            MemoryCategory::UserFact,
        ),
        (
            "m-eval-08",
            "用户的猫叫年糕，是一只橘猫，喜欢趴在窗台晒太阳",
            MemoryImportance::High,
            FRESH,
            MemoryCategory::UserFact,
        ),
        (
            "m-eval-09",
            "角色与用户约定每周三晚上一起跑步，下雨就改到健身房",
            MemoryImportance::Normal,
            AGE_14D,
            MemoryCategory::Commitment,
        ),
        (
            "m-eval-10",
            "用户不喜欢吃辣，火锅只点清汤锅底",
            MemoryImportance::Normal,
            AGE_30D,
            MemoryCategory::UserPreference,
        ),
    ];
    for (index, (memory_id, content, importance, time, category)) in corpus.iter().enumerate() {
        create_memory(
            repository,
            scope,
            memory_id,
            &format!("r-eval-{:02}", index + 1),
            *category,
            content,
            *importance,
            time,
        );
    }
}

/// 固定英文语料：与中文语料保持相同类别、权重和新鲜度梯度，并覆盖大小写、
/// 重音字符、混合语言和无关高权重项。
fn seed_english_eval_corpus(repository: &SqliteMemoryRepository, scope: &MemoryPersonaScope) {
    let corpus = [
        (
            "m-en-01",
            "The user prefers oatmeal with blueberries for breakfast",
            MemoryImportance::High,
            FRESH,
            MemoryCategory::UserPreference,
        ),
        (
            "m-en-02",
            "The user prefers black coffee from the corner café at breakfast",
            MemoryImportance::Normal,
            FRESH,
            MemoryCategory::UserPreference,
        ),
        (
            "m-en-03",
            "The user prefers toast with butter for breakfast on weekends",
            MemoryImportance::Normal,
            AGE_60D,
            MemoryCategory::UserPreference,
        ),
        (
            "m-en-04",
            "The user remembers childhood pancakes on Sunday mornings",
            MemoryImportance::Low,
            AGE_90D,
            MemoryCategory::SharedExperience,
        ),
        (
            "m-en-05",
            "The user ate trail mix after an early mountain hike",
            MemoryImportance::Normal,
            FRESH,
            MemoryCategory::SharedExperience,
        ),
        (
            "m-en-06",
            "The character and user promised to watch the next meteor shower together",
            MemoryImportance::Low,
            AGE_7D,
            MemoryCategory::StoryState,
        ),
        (
            "m-en-07",
            "The user studies Rust ownership after dinner for one hour",
            MemoryImportance::Normal,
            AGE_3D,
            MemoryCategory::UserFact,
        ),
        (
            "m-en-08",
            "The user's cat Mochi sleeps in sunlight beside the window",
            MemoryImportance::High,
            FRESH,
            MemoryCategory::UserFact,
        ),
        (
            "m-en-09",
            "The character and user run together every Wednesday evening",
            MemoryImportance::Normal,
            AGE_14D,
            MemoryCategory::Commitment,
        ),
        (
            "m-en-10",
            "The user avoids spicy food and orders mild soup",
            MemoryImportance::Normal,
            AGE_30D,
            MemoryCategory::UserPreference,
        ),
    ];
    for (index, (memory_id, content, importance, time, category)) in corpus.iter().enumerate() {
        create_memory(
            repository,
            scope,
            memory_id,
            &format!("r-en-{:02}", index + 1),
            *category,
            content,
            *importance,
            time,
        );
    }
}

/// 分页语料：7 条同查询可命中的记忆。
fn seed_paging_corpus(
    repository: &SqliteMemoryRepository,
    scope: &MemoryPersonaScope,
    prefix: &str,
) {
    let ordinals = ["一", "二", "三", "四", "五", "六", "七"];
    for (index, ordinal) in ordinals.iter().enumerate() {
        create_memory(
            repository,
            scope,
            &format!("{prefix}-{:02}", index + 1),
            &format!("r-{prefix}-{:02}", index + 1),
            MemoryCategory::UserPreference,
            &format!("用户早餐记录第{ordinal}条：豆浆加油条"),
            MemoryImportance::Normal,
            FRESH,
        );
    }
}

#[test]
fn chinese_recall_baseline() {
    let directory = TestDirectory::new("recall");
    let repository = open_repository(&directory);
    let scope = scope("评估-persona");
    seed_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    // 多候选查询：权重排序高重要度在前，同重要度新鲜的在前。
    assert_eq!(
        retrieve_ids(&retriever, &scope, "用户早餐"),
        vec!["m-eval-01", "m-eval-02", "m-eval-03"]
    );
    // 精确语义查询只命中连续覆盖的记忆。
    assert_eq!(
        retrieve_ids(&retriever, &scope, "早餐习惯"),
        vec!["m-eval-02"]
    );
    // 标点与 emoji 在归一化后不影响召回。
    assert_eq!(
        retrieve_ids(&retriever, &scope, "山顶看日出"),
        vec!["m-eval-06"]
    );
    // 混合语言查询可命中（Rust 归一化后为 rust）。
    assert_eq!(
        retrieve_ids(&retriever, &scope, "学习rust"),
        vec!["m-eval-07"]
    );
    assert_eq!(
        retrieve_ids(&retriever, &scope, "窗台晒太阳"),
        vec!["m-eval-08"]
    );
    assert_eq!(
        retrieve_ids(&retriever, &scope, "每周三晚上"),
        vec!["m-eval-09"]
    );
    // 近义不同字不召回：没有任何记忆包含“早餐店”。
    assert!(retrieve_ids(&retriever, &scope, "早餐店").is_empty());
}

#[test]
fn english_and_unicode_recall_baseline() {
    let directory = TestDirectory::new("english-unicode");
    let repository = open_repository(&directory);
    let scope = scope("english-persona");
    seed_english_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    assert_eq!(
        retrieve_ids(&retriever, &scope, "THE USER PREFERS"),
        vec!["m-en-01", "m-en-02", "m-en-03"]
    );
    assert_eq!(retrieve_ids(&retriever, &scope, "CAFÉ"), vec!["m-en-02"]);
    assert_eq!(
        retrieve_ids(&retriever, &scope, "Rust ownership"),
        vec!["m-en-07"]
    );
    assert_eq!(
        normalize_memory_fts_query("  RUST🦀。 ").expect("Unicode 符号应被安全过滤"),
        "rust"
    );
    assert_eq!(
        normalize_memory_fts_query("ÅNGSTRÖM").expect("Unicode 字母应保留并小写"),
        "ångström"
    );
}

#[test]
fn short_or_symbol_only_query_rejected() {
    let directory = TestDirectory::new("reject");
    let repository = open_repository(&directory);
    let scope = scope("评估-persona");
    seed_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    // 有效字符不足 trigram 基线（3 字符）：稳定拒绝并要求模型改写。
    assert_eq!(
        error_code(retriever.retrieve(&request(&scope, "早餐", None, None, None, None, false))),
        MemoryErrorCode::QueryRejected
    );
    assert_eq!(
        error_code(retriever.retrieve(&request(&scope, "，。!?", None, None, None, None, false))),
        MemoryErrorCode::QueryRejected
    );
    assert_eq!(
        normalize_memory_fts_query("abc").expect("三个有效字符达到 trigram 下限"),
        "abc"
    );
    // 超长查询同样拒绝，避免无底线的候选扫描。
    let long_query = "早".repeat(MAX_QUERY_CHARS + 1);
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            &long_query,
            None,
            None,
            None,
            None,
            false
        ))),
        MemoryErrorCode::QueryRejected
    );
}

#[test]
fn scattered_match_hard_eliminated() {
    let query: Vec<char> = "早上吃早餐".chars().collect();
    let scattered: Vec<char> = "早甲上乙吃丙早丁餐".chars().collect();
    assert_eq!(longest_common_substring_len(&query, &scattered), 1);
    assert!(
        !passes_hard_relevance(&query, &scattered),
        "只凑齐离散字符的高权重内容不得进入相关候选池"
    );
}

#[test]
fn irrelevant_high_weight_never_enters() {
    let directory = TestDirectory::new("irrelevant");
    let repository = open_repository(&directory);
    let scope = scope("评估-persona");
    seed_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    // m-eval-08 是全语料最高权重（High + 最新），但与早餐无关，任何早餐查询都不得带出。
    for query in ["用户早餐", "早餐习惯", "早上吃早餐"] {
        let ids = retrieve_ids(&retriever, &scope, query);
        assert!(
            !ids.iter().any(|id| id == "m-eval-08"),
            "再高权重的无关记忆也不得进入结果: {query}"
        );
    }
}

#[test]
fn retrieval_is_pure_and_decay_is_monotonic() {
    let directory = TestDirectory::new("decay");
    let repository = open_repository(&directory);
    let scope = scope("评估-persona");
    seed_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    // 读取不产生权重：同一查询连续两次结果完全一致（含 revision 与排序）。
    let first = retriever
        .retrieve(&request(&scope, "用户早餐", None, None, None, None, false))
        .expect("第一次查询应成功");
    let second = retriever
        .retrieve(&request(&scope, "用户早餐", None, None, None, None, false))
        .expect("第二次查询应成功");
    assert_eq!(first.items, second.items);

    // 时效衰减纯计算：同龄同衰减，年长者权重低；零龄不衰减，负龄钳制。
    assert_eq!(freshness_decay(0), 1.0);
    assert_eq!(freshness_decay(-1), 1.0);
    let half_life_micros = (FRESHNESS_HALF_LIFE_SECONDS * 1_000_000.0) as i64;
    assert!((freshness_decay(half_life_micros) - 0.5).abs() < 1e-9);
    assert!(freshness_decay(AGE_60D_MICROS) < freshness_decay(AGE_3D_MICROS));

    // importance 受限枚举映射单调。
    assert!(importance_weight(MemoryImportance::Low) < importance_weight(MemoryImportance::Normal));
    assert!(
        importance_weight(MemoryImportance::Normal) < importance_weight(MemoryImportance::High)
    );
}

const AGE_3D_MICROS: i64 = 3 * 24 * 3600 * 1_000_000;
const AGE_60D_MICROS: i64 = 60 * 24 * 3600 * 1_000_000;

#[test]
fn pagination_snapshot_survives_concurrent_updates() {
    let directory = TestDirectory::new("paging");
    let repository = Arc::new(open_repository(&directory));
    let scope = scope("分页-persona");
    seed_paging_corpus(&repository, &scope, "m-page");
    let retriever = SqliteMemoryRetriever::new(Arc::clone(&repository));

    let page1 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    assert_eq!(page1.items.len(), 3);
    assert!(page1.has_more);
    assert!(
        estimated_memory_query_page_tokens(&page1).expect("第一页 Token 估算应成功")
            <= MEMORY_QUERY_PAGE_TOKEN_BUDGET
    );
    let cursor = page1.next_cursor.clone().expect("应有下一页游标");

    // 第一页返回后并发写入：新增一条同查询候选，并改写全部 7 条正文与 revision。
    let start = Arc::new(Barrier::new(2));
    let writer_repository = Arc::clone(&repository);
    let writer_scope = scope.clone();
    let writer_start = Arc::clone(&start);
    let writer = std::thread::spawn(move || {
        writer_start.wait();
        create_memory(
            &writer_repository,
            &writer_scope,
            "m-page-08",
            "r-page-08",
            MemoryCategory::UserPreference,
            "用户早餐记录第八条：玉米粥",
            MemoryImportance::Normal,
            T2,
        );
        let ordinals = ["一", "二", "三", "四", "五", "六", "七"];
        for (index, ordinal) in ordinals.iter().enumerate() {
            update_memory(
                &writer_repository,
                &writer_scope,
                &format!("m-page-{:02}", index + 1),
                &format!("r-m-page-{:02}", index + 1),
                &format!("r-m-page-u{:02}", index + 1),
                &format!("用户晚餐改写第{ordinal}条"),
                false,
                T2,
            );
        }
    });
    start.wait();

    // 后续翻页仍读冻结快照：旧 revision、旧正文，新写入不可见。
    let page2 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            Some(cursor),
            None,
            None,
            false,
        ))
        .expect("第二页应成功");
    writer.join().expect("并发更新线程应成功");
    assert_eq!(page2.items.len(), 3);
    assert!(page2.has_more);
    assert!(
        estimated_memory_query_page_tokens(&page2).expect("第二页 Token 估算应成功")
            <= MEMORY_QUERY_PAGE_TOKEN_BUDGET
    );
    let page3 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            page2.next_cursor.clone(),
            None,
            None,
            false,
        ))
        .expect("第三页应成功");
    assert_eq!(page3.items.len(), 1);
    assert!(!page3.has_more);
    assert!(page3.next_cursor.is_none());
    assert!(
        estimated_memory_query_page_tokens(&page3).expect("第三页 Token 估算应成功")
            <= MEMORY_QUERY_PAGE_TOKEN_BUDGET
    );

    let mut page1_ids: Vec<String> = page1
        .items
        .iter()
        .map(|item| item.memory_id.0.clone())
        .collect();
    let mut later_ids: Vec<String> = page2
        .items
        .iter()
        .chain(page3.items.iter())
        .map(|item| item.memory_id.0.clone())
        .collect();
    for id in &later_ids {
        assert!(!page1_ids.contains(id), "翻页不得重复");
    }
    page1_ids.append(&mut later_ids);
    page1_ids.sort();
    assert_eq!(
        page1_ids,
        vec![
            "m-page-01",
            "m-page-02",
            "m-page-03",
            "m-page-04",
            "m-page-05",
            "m-page-06",
            "m-page-07"
        ],
        "冻结快照应覆盖且仅覆盖查询时的 7 条"
    );
    for item in page2.items.iter().chain(page3.items.iter()) {
        assert!(
            item.content.starts_with("用户早餐记录"),
            "后续写入不得影响已冻结页: {}",
            item.content
        );
        assert!(
            item.revision_id.0.starts_with("r-m-page-0"),
            "冻结页应保持旧 revision: {}",
            item.revision_id.0
        );
    }

    // 新查询看得到后续写入。
    let mut rewritten = Vec::new();
    let mut cursor = None;
    loop {
        let page = retriever
            .retrieve(&request(
                &scope,
                "用户晚餐改写",
                Some(MAX_MEMORY_QUERY_PAGE_SIZE as u32),
                cursor,
                None,
                None,
                false,
            ))
            .expect("新查询分页应成功");
        rewritten.extend(page.items.into_iter().map(|item| item.memory_id.0));
        let Some(next_cursor) = page.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }
    assert_eq!(rewritten.len(), 7, "新查询应看到改写后的全部记忆");
}

#[test]
fn cursor_tampered_rejected() {
    let directory = TestDirectory::new("tamper");
    let repository = open_repository(&directory);
    let scope = scope("游标-persona");
    seed_paging_corpus(&repository, &scope, "m-cur");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page1 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    let cursor = page1.next_cursor.expect("应有游标");
    let mut tampered = cursor.as_str().to_string();
    let last = tampered.pop().expect("游标非空");
    tampered.push(if last == 'a' { 'b' } else { 'a' });
    let tampered = MemoryCursor::from_runtime(tampered).expect("篡改串仍是合法 token 形态");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            Some(tampered),
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );

    let mut position_parts: Vec<&str> = cursor.as_str().split('.').collect();
    position_parts[2] = "4";
    let position_tampered =
        MemoryCursor::from_runtime(position_parts.join(".")).expect("位置篡改后仍符合 token 形态");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            Some(position_tampered),
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );

    let mut snapshot_parts: Vec<String> = cursor.as_str().split('.').map(str::to_string).collect();
    let replacement = if snapshot_parts[1].starts_with('a') {
        "b"
    } else {
        "a"
    };
    snapshot_parts[1].replace_range(..1, replacement);
    let snapshot_tampered =
        MemoryCursor::from_runtime(snapshot_parts.join(".")).expect("快照篡改后仍符合 token 形态");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            Some(snapshot_tampered),
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );
}

#[test]
fn cursor_expired_by_ttl() {
    let directory = TestDirectory::new("ttl");
    let repository = open_repository(&directory);
    let scope = scope("游标-persona");
    seed_paging_corpus(&repository, &scope, "m-cur");
    let retriever = SqliteMemoryRetriever::with_cursor_ttl_seconds(Arc::new(repository), -1);

    let page1 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            page1.next_cursor,
            None,
            None,
            false,
        ))),
        MemoryErrorCode::CursorExpired
    );
}

#[test]
fn cursor_cross_persona_rejected() {
    let directory = TestDirectory::new("persona");
    let repository = open_repository(&directory);
    let scope_a = scope("游标-persona");
    seed_paging_corpus(&repository, &scope_a, "m-cur");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page1 = retriever
        .retrieve(&request(
            &scope_a,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    let scope_b = scope("其他-persona");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope_b,
            "用户早餐记录",
            Some(3),
            page1.next_cursor,
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );
}

#[test]
fn cursor_mixed_with_new_query_rejected() {
    let directory = TestDirectory::new("mixed");
    let repository = open_repository(&directory);
    let scope = scope("游标-persona");
    seed_paging_corpus(&repository, &scope, "m-cur");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page1 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "每周三晚上",
            Some(3),
            page1.next_cursor,
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );
}

#[test]
fn cursor_binds_as_of_history_and_memory_id() {
    let directory = TestDirectory::new("cursor-filters");
    let repository = open_repository(&directory);
    let scope = scope("游标-persona");
    seed_paging_corpus(&repository, &scope, "m-filter");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let as_of_page = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(2),
            None,
            Some("2026-07-30T12:00:00Z"),
            None,
            false,
        ))
        .expect("as_of 第一页应成功");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(2),
            as_of_page.next_cursor,
            None,
            None,
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );

    let repository = retriever.repository.as_ref();
    create_memory(
        repository,
        &scope,
        "m-history-bind",
        "r-history-bind-01",
        MemoryCategory::UserPreference,
        "历史绑定初始内容",
        MemoryImportance::Normal,
        "2026-07-30T01:00:00Z",
    );
    for index in 2..=7 {
        update_memory(
            repository,
            &scope,
            "m-history-bind",
            &format!("r-history-bind-{:02}", index - 1),
            &format!("r-history-bind-{index:02}"),
            &format!("历史绑定第 {index} 次更新"),
            false,
            &format!("2026-07-30T{index:02}:00:00Z"),
        );
    }
    let history_page = retriever
        .retrieve(&request(
            &scope,
            "历史绑定",
            Some(2),
            None,
            None,
            Some("m-history-bind"),
            true,
        ))
        .expect("历史第一页应成功");
    let history_cursor = history_page.next_cursor.expect("历史应有续页游标");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "历史绑定",
            Some(2),
            Some(history_cursor.clone()),
            None,
            Some("m-history-bind"),
            false,
        ))),
        MemoryErrorCode::InvalidCursor
    );
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "历史绑定",
            Some(2),
            Some(history_cursor),
            None,
            Some("m-other"),
            true,
        ))),
        MemoryErrorCode::InvalidCursor
    );
}

#[test]
fn cursor_expired_at_turn_boundary() {
    let directory = TestDirectory::new("boundary");
    let repository = open_repository(&directory);
    let scope = scope("游标-persona");
    seed_paging_corpus(&repository, &scope, "m-cur");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page1 = retriever
        .retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("第一页应成功");
    // Turn 结束：运行时使本 Turn 全部快照失效。
    retriever
        .expire_cursor(page1.next_cursor.as_ref().expect("应有游标"))
        .expect("Turn 边界失效应成功");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐记录",
            Some(3),
            page1.next_cursor,
            None,
            None,
            false,
        ))),
        MemoryErrorCode::CursorExpired
    );
}

#[test]
fn turn_boundary_expiration_does_not_affect_concurrent_turn() {
    let directory = TestDirectory::new("boundary-isolation");
    let repository = open_repository(&directory);
    let scope_a = scope("turn-a-persona");
    let scope_b = scope("turn-b-persona");
    seed_paging_corpus(&repository, &scope_a, "m-turn-a");
    seed_paging_corpus(&repository, &scope_b, "m-turn-b");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page_a = retriever
        .retrieve(&request(
            &scope_a,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("Turn A 第一页应成功");
    let page_b = retriever
        .retrieve(&request(
            &scope_b,
            "用户早餐记录",
            Some(3),
            None,
            None,
            None,
            false,
        ))
        .expect("Turn B 第一页应成功");
    let cursor_a = page_a.next_cursor.expect("Turn A 应有游标");
    let cursor_b = page_b.next_cursor.expect("Turn B 应有游标");

    retriever
        .expire_cursor(&cursor_a)
        .expect("Turn A 边界失效应成功");
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope_a,
            "用户早餐记录",
            Some(3),
            Some(cursor_a),
            None,
            None,
            false,
        ))),
        MemoryErrorCode::CursorExpired
    );
    let continued_b = retriever
        .retrieve(&request(
            &scope_b,
            "用户早餐记录",
            Some(3),
            Some(cursor_b),
            None,
            None,
            false,
        ))
        .expect("Turn A 结束不得使并发 Turn B 过期");
    assert_eq!(continued_b.items.len(), 3);
}

#[test]
fn corrected_never_enters_model_surface() {
    let directory = TestDirectory::new("corrected");
    let repository = open_repository(&directory);
    let scope = scope("纠正-persona");
    create_memory(
        &repository,
        &scope,
        "m-correct-01",
        "r-correct-01",
        MemoryCategory::UserPreference,
        "用户早餐喜欢吃豆浆油条",
        MemoryImportance::Normal,
        T1,
    );
    update_memory(
        &repository,
        &scope,
        "m-correct-01",
        "r-correct-01",
        "r-correct-02",
        "用户早餐喜欢吃面包牛奶",
        false,
        T2,
    );
    update_memory(
        &repository,
        &scope,
        "m-correct-01",
        "r-correct-02",
        "r-correct-03",
        "用户早餐其实吃燕麦粥",
        true,
        T3,
    );
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    // 相关性查询只暴露当前 revision。
    let receipt = retriever
        .retrieve(&request(&scope, "用户早餐", None, None, None, None, false))
        .expect("查询应成功");
    assert_eq!(receipt.items.len(), 1);
    assert_eq!(receipt.items[0].revision_id.0, "r-correct-03");
    assert_eq!(receipt.items[0].content, "用户早餐其实吃燕麦粥");

    // 显式历史：current + superseded，带变化语义；corrected 永不出现。
    let history = retriever
        .retrieve(&request(
            &scope,
            "用户早餐",
            None,
            None,
            None,
            Some("m-correct-01"),
            true,
        ))
        .expect("历史查询应成功");
    assert_eq!(history.items.len(), 2, "corrected 不得进入模型读取面");
    assert_eq!(history.items[0].revision_id.0, "r-correct-03");
    assert_eq!(history.items[0].change_type, MemoryChangeType::Correct);
    assert!(history.items[0].valid_to.is_none());
    assert_eq!(history.items[1].revision_id.0, "r-correct-01");
    assert_eq!(history.items[1].change_type, MemoryChangeType::Create);
    assert_eq!(history.items[1].valid_to.as_deref(), Some(T2));
    assert!(!history.items[1].change_reason.is_empty());

    // as_of 落在 superseded 区间：返回当时的可读 revision。
    let at_superseded = retriever
        .retrieve(&request(
            &scope,
            "用户早餐",
            None,
            None,
            Some("2026-07-30T01:30:00Z"),
            Some("m-correct-01"),
            false,
        ))
        .expect("as_of 查询应成功");
    assert_eq!(at_superseded.items.len(), 1);
    assert_eq!(at_superseded.items[0].revision_id.0, "r-correct-01");
    assert_eq!(at_superseded.items[0].content, "用户早餐喜欢吃豆浆油条");

    // as_of 落在 corrected 区间：该时刻没有模型可读 revision，返回空页而非旧错误内容。
    let at_corrected = retriever
        .retrieve(&request(
            &scope,
            "用户早餐",
            None,
            None,
            Some("2026-07-30T02:30:00Z"),
            Some("m-correct-01"),
            false,
        ))
        .expect("as_of 查询应成功");
    assert!(at_corrected.items.is_empty());
    assert!(!at_corrected.has_more);
}

#[test]
fn history_mode_requires_memory_id_and_rejects_as_of() {
    let directory = TestDirectory::new("histreq");
    let repository = open_repository(&directory);
    let scope = scope("纠正-persona");
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    assert_eq!(
        error_code(retriever.retrieve(&request(&scope, "用户早餐", None, None, None, None, true))),
        MemoryErrorCode::QueryRejected
    );
    assert_eq!(
        error_code(retriever.retrieve(&request(
            &scope,
            "用户早餐",
            None,
            None,
            Some(T2),
            Some("m-correct-01"),
            true,
        ))),
        MemoryErrorCode::QueryRejected
    );
}

#[test]
fn revision_history_includes_corrected_for_management() {
    let directory = TestDirectory::new("history");
    let repository = open_repository(&directory);
    let persona_scope = scope("纠正-persona");
    create_memory(
        &repository,
        &persona_scope,
        "m-correct-01",
        "r-correct-01",
        MemoryCategory::UserPreference,
        "用户早餐喜欢吃豆浆油条",
        MemoryImportance::Normal,
        T1,
    );
    update_memory(
        &repository,
        &persona_scope,
        "m-correct-01",
        "r-correct-01",
        "r-correct-02",
        "用户早餐喜欢吃面包牛奶",
        false,
        T2,
    );
    update_memory(
        &repository,
        &persona_scope,
        "m-correct-01",
        "r-correct-02",
        "r-correct-03",
        "用户早餐其实吃燕麦粥",
        true,
        T3,
    );

    // 管理审计读取含 corrected，按 valid_from 升序。
    let history = repository
        .revision_history(&persona_scope, &MemoryId("m-correct-01".to_string()))
        .expect("管理历史应可读");
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].revision_id.0, "r-correct-01");
    assert_eq!(history[0].state, MemoryRevisionState::Superseded);
    assert_eq!(history[1].revision_id.0, "r-correct-02");
    assert_eq!(history[1].state, MemoryRevisionState::Corrected);
    assert_eq!(history[2].revision_id.0, "r-correct-03");
    assert_eq!(history[2].state, MemoryRevisionState::Current);

    // 跨 Persona 与未知记忆都按不存在处理。
    let other = scope("其他-persona");
    assert_eq!(
        repository
            .revision_history(&other, &MemoryId("m-correct-01".to_string()))
            .expect_err("跨 Persona 应拒绝")
            .code(),
        MemoryErrorCode::MemoryNotFound
    );
    assert_eq!(
        repository
            .revision_history(&persona_scope, &MemoryId("m-unknown".to_string()))
            .expect_err("未知记忆应拒绝")
            .code(),
        MemoryErrorCode::MemoryNotFound
    );
}

#[test]
fn active_memory_count_is_persona_scoped_and_revision_stable() {
    let directory = TestDirectory::new("active-count");
    let repository = open_repository(&directory);
    let scope_a = scope("count-persona-a");
    let scope_b = scope("count-persona-b");

    for index in 1..=3 {
        create_memory(
            &repository,
            &scope_a,
            &format!("m-count-a-{index}"),
            &format!("r-count-a-{index}"),
            MemoryCategory::UserFact,
            &format!("Persona A 的第 {index} 条有效记忆"),
            MemoryImportance::Normal,
            T1,
        );
    }
    create_memory(
        &repository,
        &scope_b,
        "m-count-b-1",
        "r-count-b-1",
        MemoryCategory::UserFact,
        "Persona B 的唯一有效记忆",
        MemoryImportance::Normal,
        T1,
    );
    update_memory(
        &repository,
        &scope_a,
        "m-count-a-1",
        "r-count-a-1",
        "r-count-a-1-u",
        "Persona A 的第一条记忆已经更新",
        false,
        T2,
    );

    assert_eq!(
        repository
            .active_memory_count(&scope_a)
            .expect("Persona A 计数应成功"),
        3,
        "新增 revision 不得被误计为新增逻辑记忆"
    );
    assert_eq!(
        repository
            .active_memory_count(&scope_b)
            .expect("Persona B 计数应成功"),
        1
    );
    assert_eq!(
        repository
            .active_memory_count(&scope("count-persona-empty"))
            .expect("空 Persona 计数应成功"),
        0
    );
}

#[test]
fn scoring_and_paging_primitives() {
    // 连续覆盖率基础：最长公共连续子串。
    assert_eq!(
        longest_common_substring_len(
            &"早餐习惯".chars().collect::<Vec<_>>(),
            &"用餐习惯很好早餐也规律".chars().collect::<Vec<_>>()
        ),
        3
    );
    assert_eq!(
        longest_common_substring_len(
            &"早上吃早餐".chars().collect::<Vec<_>>(),
            &"用户早上爬山到山上吃早晨带的压缩饼干早餐留到中午才吃"
                .chars()
                .collect::<Vec<_>>()
        ),
        3
    );

    // Token 估计：CJK 按 1，ASCII 按 1/4。
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("早餐"), 2);
    assert_eq!(estimate_tokens("ab早餐"), 3);

    // 页大小解析：默认值与上限均由集中常量控制。
    let params = |limit| MemoryQueryParams {
        query: "用户早餐".to_string(),
        limit,
        cursor: None,
        as_of: None,
        memory_id: None,
        include_history: false,
    };
    assert_eq!(page_size(&params(None)), DEFAULT_MEMORY_QUERY_PAGE_SIZE);
    assert_eq!(page_size(&params(Some(3))), 3);
    assert_eq!(page_size(&params(Some(1000))), MAX_MEMORY_QUERY_PAGE_SIZE);

    // 同权重排序：相关度（bm25 低者优）优先，其次更新时间，最后稳定 ID。
    let entry = MemoryEntry {
        memory_id: MemoryId("m-x".to_string()),
        persona_id: "p".to_string(),
        category: MemoryCategory::UserFact,
        current_revision_id: MemoryRevisionId("r-x".to_string()),
        importance: MemoryImportance::Normal,
        freshness_at: FRESH.to_string(),
        created_at: FRESH.to_string(),
        state: MemoryEntryState::Active,
    };
    let revision = |id: &str| MemoryRevision {
        revision_id: MemoryRevisionId(id.to_string()),
        memory_id: MemoryId("m-x".to_string()),
        content: "内容".to_string(),
        event_time: None,
        recorded_at: FRESH.to_string(),
        valid_from: FRESH.to_string(),
        valid_to: None,
        change_type: MemoryChangeType::Create,
        change_reason: "原因".to_string(),
        source: crate::domain::memory::MemorySourceEvidence::ConversationTurn {
            conversation_id: "c".to_string(),
            turn_id: "t".to_string(),
            kind: MemorySourceKind::DirectUserMessage,
        },
        safety_policy_version: "v".to_string(),
        state: MemoryRevisionState::Current,
    };
    let item = |memory_id: &str, weight, relevance, freshness_micros| {
        let mut item =
            FrozenItem::new(&entry, revision("r-x"), weight, relevance, 0).expect("条目应构造成功");
        item.memory_id = MemoryId(memory_id.to_string());
        item.freshness_micros = freshness_micros;
        item
    };
    let mut items = [
        item("m-b", 2.0, -1.0, 0),
        item("m-a", 2.0, -1.0, 0),
        item("m-c", 2.0, -2.0, 0),
        item("m-d", 3.0, -0.5, 0),
    ];
    items.sort_by(compare_frozen_items);
    let ordered: Vec<&str> = items.iter().map(|item| item.memory_id.0.as_str()).collect();
    assert_eq!(ordered, vec!["m-d", "m-c", "m-a", "m-b"]);

    // 游标解析：合法形态往返成功，畸形一律 memory_cursor_invalid。
    let legal = format!("mqc1.0123456789abcdef.3.1900000000.{}", "0".repeat(64));
    let parsed = parse_cursor(&legal).expect("合法游标应解析成功");
    assert_eq!(parsed.snapshot_id, "0123456789abcdef");
    assert_eq!(parsed.position, 3);
    for malformed in [
        "",
        "mqc1",
        "mqc1.xyz.3.1900000000.0000",
        "v2.0123456789abcdef.3.1.0000",
    ] {
        assert_eq!(
            parse_cursor(malformed).expect_err("畸形游标应拒绝").code(),
            MemoryErrorCode::InvalidCursor
        );
    }
}

#[test]
fn page_token_budget_is_hard_and_skips_oversized_item() {
    let directory = TestDirectory::new("page-token-budget");
    let repository = open_repository(&directory);
    let scope = scope("budget-persona");
    create_memory(
        &repository,
        &scope,
        "m-budget-oversized",
        "r-budget-oversized",
        MemoryCategory::UserFact,
        &"预算样本".repeat(300),
        MemoryImportance::High,
        FRESH,
    );
    create_memory(
        &repository,
        &scope,
        "m-budget-normal",
        "r-budget-normal",
        MemoryCategory::UserFact,
        "预算样本正常条目",
        MemoryImportance::Normal,
        FRESH,
    );
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    let page = retriever
        .retrieve(&request(&scope, "预算样本", None, None, None, None, false))
        .expect("预算查询应成功");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].memory_id.0, "m-budget-normal");
    assert!(
        estimated_memory_query_page_tokens(&page).expect("页 Token 估算应成功")
            <= MEMORY_QUERY_PAGE_TOKEN_BUDGET
    );
}

#[test]
fn turn_query_budget_rejects_over_call_and_token_limits() {
    let directory = TestDirectory::new("turn-budget");
    let repository = open_repository(&directory);
    let scope = scope("budget-persona");
    create_memory(
        &repository,
        &scope,
        "m-budget-normal",
        "r-budget-normal",
        MemoryCategory::UserFact,
        "预算样本正常条目",
        MemoryImportance::Normal,
        FRESH,
    );
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));
    let page = retriever
        .retrieve(&request(&scope, "预算样本", None, None, None, None, false))
        .expect("预算查询应成功");

    let mut budget = MemoryQueryBudget::new();
    for _ in 0..MEMORY_QUERY_TURN_MAX_CALLS {
        budget.reserve_query_call().expect("预算内调用应允许保留");
        budget
            .consume_page_tokens(&page)
            .expect("预算内页面应允许消费");
    }
    assert_eq!(budget.consumed_calls(), MEMORY_QUERY_TURN_MAX_CALLS);
    assert!(budget.consumed_tokens() <= MEMORY_QUERY_TURN_TOKEN_BUDGET);
    assert_eq!(
        budget
            .reserve_query_call()
            .expect_err("超过 Turn 调用上限应拒绝")
            .code(),
        MemoryErrorCode::QueryBudgetExceeded
    );

    let mut oversized_page = page.clone();
    oversized_page.items[0].content = "超长预算正文".repeat(500);
    let mut fresh_budget = MemoryQueryBudget::new();
    fresh_budget
        .reserve_query_call()
        .expect("首次调用应允许保留");
    assert_eq!(
        fresh_budget
            .consume_page_tokens(&oversized_page)
            .expect_err("超过单页 Token 上限应拒绝")
            .code(),
        MemoryErrorCode::QueryBudgetExceeded
    );
    assert_eq!(
        fresh_budget.consumed_calls(),
        1,
        "失败结果也必须消耗调用预算"
    );
    assert_eq!(
        fresh_budget.consumed_tokens(),
        0,
        "拒绝页不得消耗 Token 预算"
    );
}

/// 固定中英文语料评估：实测召回、硬选池、排序与完整收据 Token 占用。
/// 运行 `cargo test -p muse-core fixed_bilingual_corpus_evaluation -- --nocapture` 可复现。
#[test]
fn fixed_bilingual_corpus_evaluation() {
    let directory = TestDirectory::new("evaluation");
    let repository = open_repository(&directory);
    let scope = scope("评估-persona");
    seed_eval_corpus(&repository, &scope);
    seed_english_eval_corpus(&repository, &scope);
    let retriever = SqliteMemoryRetriever::new(Arc::new(repository));

    println!(
        "EVAL|corpus=20 memories language=zh-CN,en persona=评估-persona tokenizer=trigram sqlite=bundled"
    );

    let queries = [
        ("早餐", None, "两字短查询：有效字符不足 trigram 基线"),
        ("用户早餐", Some(3), "多候选：权重主序 + 衰减"),
        ("早餐习惯", Some(1), "精确语义：连续覆盖"),
        ("早上吃早餐", Some(0), "散乱凑字：硬淘汰"),
        ("山顶看日出", Some(1), "标点 + emoji 归一化"),
        ("学习rust", Some(1), "混合语言"),
        ("每周三晚上", Some(1), "约定类召回"),
        ("早餐店", Some(0), "近义不同字：不召回"),
        ("THE USER PREFERS", Some(3), "英文大小写 + 权重排序"),
        ("CAFÉ", Some(1), "Unicode 重音字符"),
        ("Rust ownership", Some(1), "英文技术词组"),
        ("meteor shower", Some(1), "英文共同经历"),
    ];
    for (query, expected, note) in queries {
        let outcome = retriever.retrieve(&request(&scope, query, None, None, None, None, false));
        match (outcome, expected) {
            (Ok(receipt), Some(expected_count)) => {
                assert_eq!(
                    receipt.items.len(),
                    expected_count,
                    "查询 {query} 命中数不符合评估基线"
                );
                let ids: Vec<&str> = receipt
                    .items
                    .iter()
                    .map(|item| item.memory_id.0.as_str())
                    .collect();
                println!("EVAL|query={query} kept={expected_count} order={ids:?} note={note}");
            }
            (Err(error), None) => {
                assert_eq!(error.code(), MemoryErrorCode::QueryRejected);
                println!("EVAL|query={query} rejected=memory_query_rejected note={note}");
            }
            (other, expected) => {
                panic!("查询 {query} 结果与评估基线不符: {other:?} vs {expected:?}")
            }
        }
    }

    // 排序质量与权重证据：直接读取冻结条目的内部排序键。
    let normalized = normalize_memory_fts_query("用户早餐").expect("查询应可规范化");
    let params = MemoryQueryParams {
        query: "用户早餐".to_string(),
        limit: None,
        cursor: None,
        as_of: None,
        memory_id: None,
        include_history: false,
    };
    let evaluation_now = chrono::DateTime::parse_from_rfc3339(FRESH)
        .expect("固定评估时间应有效")
        .with_timezone(&Utc);
    let items = retriever
        .build_frozen_items(&scope, &params, &normalized, evaluation_now)
        .expect("冻结构建应成功");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].memory_id.0, "m-eval-01");
    assert_eq!(items[1].memory_id.0, "m-eval-02");
    assert_eq!(items[2].memory_id.0, "m-eval-03");
    assert!(items[0].effective_weight > items[1].effective_weight);
    assert!(items[1].effective_weight > items[2].effective_weight);
    for item in &items {
        println!(
            "EVAL|rank id={} importance={:?} weight={:.4} tokens={}",
            item.memory_id.0,
            item.importance,
            item.effective_weight,
            item.estimated_tokens().expect("条目 Token 估算应成功")
        );
    }

    // Token 证据按完整模型可见 JSON 收据估算，覆盖中英文各 10 条。
    let memory_ids = (1..=10)
        .map(|index| format!("m-eval-{index:02}"))
        .chain((1..=10).map(|index| format!("m-en-{index:02}")));
    let mut direct_page_estimates = Vec::new();
    for memory_id in memory_ids {
        let direct = retriever
            .retrieve(&request(
                &scope,
                "direct lookup",
                None,
                None,
                None,
                Some(&memory_id),
                false,
            ))
            .expect("直接读取应成功");
        assert_eq!(direct.items.len(), 1);
        let tokens = estimated_memory_query_page_tokens(&direct).expect("完整页 Token 估算应成功");
        direct_page_estimates.push(tokens);
        println!("EVAL|direct_page_tokens id={memory_id} tokens={tokens}");
    }
    let total: usize = direct_page_estimates.iter().sum();
    let max = direct_page_estimates.iter().max().copied().unwrap_or(0);
    let average = total / direct_page_estimates.len();
    println!("EVAL|direct_page_tokens avg={average} max={max} total={total}");
    assert!(
        max <= MEMORY_QUERY_PAGE_TOKEN_BUDGET,
        "固定语料最长单条应能被单页预算容纳"
    );

    let chinese_page = retriever
        .retrieve(&request(&scope, "用户早餐", None, None, None, None, false))
        .expect("中文默认页应成功");
    let english_page = retriever
        .retrieve(&request(
            &scope,
            "THE USER PREFERS",
            None,
            None,
            None,
            None,
            false,
        ))
        .expect("英文默认页应成功");
    let chinese_page_tokens =
        estimated_memory_query_page_tokens(&chinese_page).expect("中文页 Token 估算应成功");
    let english_page_tokens =
        estimated_memory_query_page_tokens(&english_page).expect("英文页 Token 估算应成功");
    assert!(chinese_page_tokens <= MEMORY_QUERY_PAGE_TOKEN_BUDGET);
    assert!(english_page_tokens <= MEMORY_QUERY_PAGE_TOKEN_BUDGET);
    println!(
        "EVAL|default_pages zh_items={} zh_tokens={} en_items={} en_tokens={}",
        chinese_page.items.len(),
        chinese_page_tokens,
        english_page.items.len(),
        english_page_tokens
    );
    println!(
        "EVAL|recommendation default_page_size={} max_page_size={} page_token_budget={} \
         turn_query_budget_calls={} turn_query_budget_tokens={} ttl_seconds={}",
        DEFAULT_MEMORY_QUERY_PAGE_SIZE,
        MAX_MEMORY_QUERY_PAGE_SIZE,
        MEMORY_QUERY_PAGE_TOKEN_BUDGET,
        MEMORY_QUERY_TURN_MAX_CALLS,
        MEMORY_QUERY_TURN_TOKEN_BUDGET,
        CURSOR_TTL_SECONDS
    );
}
