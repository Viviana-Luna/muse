//! Persona 长期记忆的确定性敏感检测器，实现 `MemorySensitivityPolicy` 端口。
//!
//! 设计边界：
//! - 规则全部编译期内置、纯函数求值，不存在"规则加载失败后可放行"的路径；
//!   判定输入缺失（正文为空）等不确定状态一律 fail closed。
//! - 命中规则只能增加拒绝，任何规则都不授予额外权限；模型提议、用户显式要求、
//!   手工编辑与恢复导入都经过同一实现。
//! - 检测只面向记忆派生层；普通 Session 原文仍按既有会话留存契约保存。

use crate::domain::memory::{
    MemorySafetyAssessment, MemorySafetyFailure, MemorySensitivityPolicy, MemorySensitivityRequest,
};

/// 当前确定性规则集版本；规则变化必须同步升级，持久化 revision 凭它审计。
pub const MEMORY_SENSITIVITY_POLICY_VERSION: &str = "deterministic-v1";

/// 确定性敏感检测器；无状态，可跨线程共享。
#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicMemorySensitivityPolicy;

impl DeterministicMemorySensitivityPolicy {
    pub const fn new() -> Self {
        Self
    }
}

impl MemorySensitivityPolicy for DeterministicMemorySensitivityPolicy {
    fn assess(&self, request: MemorySensitivityRequest<'_>) -> MemorySafetyAssessment {
        // 判定输入缺失时无法得出允许结论，fail closed。
        if request.content.trim().is_empty() || request.change_reason.trim().is_empty() {
            return MemorySafetyAssessment::FailClosed {
                stage: request.stage,
                reason: MemorySafetyFailure::DecisionMissing,
            };
        }
        // content 与 change_reason 分别判定，任一命中即整项拒绝；不拼接扫描，
        // 避免一字段末尾的关键词把另一字段误读为自己的上下文。
        let rejected = hits_sensitive_rule(request.content).is_some()
            || hits_sensitive_rule(request.change_reason).is_some();
        if rejected {
            MemorySafetyAssessment::Rejected {
                stage: request.stage,
                policy_version: Some(MEMORY_SENSITIVITY_POLICY_VERSION.to_string()),
            }
        } else {
            MemorySafetyAssessment::Allowed {
                stage: request.stage,
                policy_version: MEMORY_SENSITIVITY_POLICY_VERSION.to_string(),
            }
        }
    }
}

/// 命中的敏感类别；只用于内部判定与测试断言，不写入日志或事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SensitiveCategory {
    HardSecret,
    IdentityDocument,
    FinancialPayment,
    PreciseLocation,
    PrivateContact,
    HealthMedical,
    SexualIntimacy,
    Minor,
    ThirdPartyPrivacy,
}

type SensitiveRule = fn(&str, &str) -> bool;

fn hits_sensitive_rule(text: &str) -> Option<SensitiveCategory> {
    let lower = text.to_lowercase();
    let rules: [(SensitiveCategory, SensitiveRule); 9] = [
        (SensitiveCategory::HardSecret, |text, lower| {
            contains_hard_secret(text, lower)
        }),
        (SensitiveCategory::IdentityDocument, |text, _| {
            contains_identity_document(text)
        }),
        (SensitiveCategory::FinancialPayment, |text, lower| {
            contains_financial_payment(text, lower)
        }),
        (SensitiveCategory::PreciseLocation, |text, _| {
            contains_precise_location(text)
        }),
        (SensitiveCategory::PrivateContact, |text, _| {
            contains_private_contact(text)
        }),
        (SensitiveCategory::HealthMedical, |text, _| {
            contains_health_medical(text)
        }),
        (SensitiveCategory::SexualIntimacy, |text, _| {
            contains_sexual_intimacy(text)
        }),
        (SensitiveCategory::Minor, |text, _| {
            contains_minor_reference(text)
        }),
        (SensitiveCategory::ThirdPartyPrivacy, |text, _| {
            contains_third_party_privacy(text)
        }),
    ];
    rules
        .iter()
        .find(|(_, rule)| rule(text, &lower))
        .map(|(category, _)| *category)
}

/// 硬秘密与认证凭据：PEM 私钥块、已知令牌前缀、密钥赋值句式。
fn contains_hard_secret(text: &str, lower: &str) -> bool {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        return true;
    }
    const TOKEN_PREFIXES: [&str; 24] = [
        "sk-ant-",
        "sk-proj-",
        "sk_live_",
        "sk_test_",
        "sk-",
        "rk_live_",
        "AKIA",
        "ASIA",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "xoxs-",
        "ya29.",
        "AIza",
        "dop_v1_",
        "hf_",
    ];
    for word in ascii_words(text) {
        for prefix in TOKEN_PREFIXES {
            if word.starts_with(prefix) && word.len() >= prefix.len() + 16 {
                return true;
            }
        }
        // JWT 固定以 eyJ 开头且含两段分隔；短词不作为凭据。
        if word.starts_with("eyJ") && word.len() >= 32 && word.matches('.').count() >= 2 {
            return true;
        }
    }
    if lower.contains("authorization: bearer ")
        || lower.contains("authorization=basic ")
        || lower.contains("cookie: session=")
        || lower.contains("set-cookie: session=")
    {
        return true;
    }
    const ASSIGNMENT_KEYS: [&str; 17] = [
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api-key",
        "secret_key",
        "secret-key",
        "client_secret",
        "access_token",
        "refresh_token",
        "id_token",
        "private_key",
        "auth_token",
        "session_token",
        "signing_key",
        "otp",
        "pin",
    ];
    if contains_assignment_pattern(lower, &ASSIGNMENT_KEYS, &["=", ":"]) {
        return true;
    }
    const CJK_CREDENTIAL_KEYS: [&str; 7] =
        ["密码", "口令", "密钥", "凭据", "令牌", "验证码", "支付码"];
    contains_assignment_pattern(text, &CJK_CREDENTIAL_KEYS, &[":", "：", "="])
}

/// 身份证明：带校验位的中国大陆身份证号，或证件关键词伴随数字编号。
fn contains_identity_document(text: &str) -> bool {
    let chars = text.chars().collect::<Vec<_>>();
    for window in chars.windows(18) {
        if window[..17].iter().all(|ch| ch.is_ascii_digit())
            && (window[17].is_ascii_digit() || matches!(window[17], 'X' | 'x'))
        {
            let candidate = window.iter().collect::<String>();
            if chinese_id_checksum_valid(&candidate) {
                return true;
            }
        }
    }
    for run in digit_runs(text) {
        if run.len() == 18 && run.chars().take(17).all(|c| c.is_ascii_digit()) {
            let tail = run.chars().next_back().unwrap_or(' ');
            if (tail.is_ascii_digit() || matches!(tail, 'X' | 'x'))
                && chinese_id_checksum_valid(&run)
            {
                return true;
            }
        }
        if run.len() == 18 && chinese_id_checksum_valid(&run) {
            return true;
        }
    }
    const DOCUMENT_KEYS: [&str; 6] = ["身份证", "护照", "社保卡", "驾驶证", "港澳通行证", "台胞证"];
    DOCUMENT_KEYS
        .iter()
        .any(|key| keyword_followed_by_digit_run(text, key, 6, 24))
}

/// 金融支付：Luhn 有效的长卡号，或金融关键词伴随数字。
fn contains_financial_payment(text: &str, lower: &str) -> bool {
    for run in digit_runs(text) {
        if (16..=19).contains(&run.len()) && luhn_valid(&run) {
            return true;
        }
    }
    const FINANCIAL_KEYS: [&str; 8] = [
        "银行卡",
        "信用卡",
        "卡号",
        "cvv",
        "支付密码",
        "收款账号",
        "银行账号",
        "支付口令",
    ];
    FINANCIAL_KEYS
        .iter()
        .any(|key| keyword_followed_by_digit_run(lower, key, 3, 24))
}

/// 精确住址与实时位置：高精度坐标对，门牌号伴随编号，或住址类关键词伴随具体尾文。
fn contains_precise_location(text: &str) -> bool {
    if contains_precise_coordinates(text) {
        return true;
    }
    if keyword_followed_by_digit_run(text, "门牌号", 1, 16) {
        return true;
    }
    const LOCATION_KEYS: [&str; 9] = [
        "详细住址",
        "家庭住址",
        "家住",
        "现居",
        "实时位置",
        "定位到",
        "经纬度",
        "GPS 坐标",
        "gps 坐标",
    ];
    LOCATION_KEYS
        .iter()
        .any(|key| keyword_with_meaningful_tail(text, key, 4))
}

/// 私人联系方式：手机号、国际号码、邮箱地址，或联系方式关键词伴随号码。
fn contains_private_contact(text: &str) -> bool {
    for run in digit_runs(text) {
        let digits = run.as_str();
        if run.len() == 11
            && digits.starts_with('1')
            && digits
                .chars()
                .nth(1)
                .is_some_and(|c| ('3'..='9').contains(&c))
        {
            return true;
        }
    }
    for word in ascii_words(text) {
        if let Some(rest) = word.strip_prefix('+')
            && (8..=15).contains(&rest.len())
            && rest.bytes().all(|byte| byte.is_ascii_digit())
        {
            return true;
        }
    }
    if contains_email_address(text) {
        return true;
    }
    const CONTACT_KEYS: [&str; 5] = ["微信号", "QQ号", "qq号", "手机号", "电话号码"];
    CONTACT_KEYS
        .iter()
        .any(|key| keyword_followed_by_digit_run(text, key, 5, 16))
}

/// 健康医疗：确定性医学敏感关键词，命中即拒绝。
fn contains_health_medical(text: &str) -> bool {
    const HEALTH_KEYS: [&str; 21] = [
        "病历",
        "诊断证明",
        "确诊",
        "处方",
        "用药",
        "病史",
        "体检报告",
        "住院",
        "癌症",
        "肿瘤",
        "艾滋",
        "HIV",
        "hiv",
        "抑郁",
        "焦虑障碍",
        "精神疾病",
        "心理咨询",
        "精神病",
        "慢性病",
        "高血压",
        "糖尿病",
    ];
    HEALTH_KEYS.iter().any(|key| text.contains(key))
}

/// 性与亲密关系：确定性敏感关键词，命中即拒绝。
fn contains_sexual_intimacy(text: &str) -> bool {
    const SEXUAL_KEYS: [&str; 9] = [
        "性生活",
        "性行为",
        "性爱",
        "做爱",
        "性关系",
        "亲密行为",
        "一夜情",
        "约炮",
        "床笫",
    ];
    SEXUAL_KEYS.iter().any(|key| text.contains(key))
}

/// 未成年人：明确未成年表述或 18 岁以下年龄。
fn contains_minor_reference(text: &str) -> bool {
    const MINOR_KEYS: [&str; 7] = [
        "未成年",
        "未满18",
        "未满十八",
        "小学生",
        "初中生",
        "适龄儿童",
        "幼儿园",
    ];
    if MINOR_KEYS.iter().any(|key| text.contains(key)) {
        return true;
    }
    age_below_eighteen(text)
}

/// 第三方隐私：第三人称关系词与敏感关键词近距离共现。
fn contains_third_party_privacy(text: &str) -> bool {
    const THIRD_PARTY_MARKERS: [&str; 22] = [
        "家人",
        "朋友",
        "同事",
        "同学",
        "领导",
        "丈夫",
        "妻子",
        "老公",
        "老婆",
        "爸爸",
        "妈妈",
        "父亲",
        "母亲",
        "孩子",
        "儿子",
        "女儿",
        "邻居",
        "室友",
        "前任",
        "女朋友",
        "男朋友",
        "女友",
    ];
    const SENSITIVE_KEYS: [&str; 13] = [
        "身份证",
        "护照",
        "手机号",
        "电话号码",
        "住址",
        "银行卡",
        "卡号",
        "病历",
        "确诊",
        "病史",
        "住院",
        "工资",
        "收入",
    ];
    for (index, _) in text.char_indices() {
        if !THIRD_PARTY_MARKERS
            .iter()
            .any(|marker| text[index..].starts_with(marker))
        {
            continue;
        }
        let window = char_window(text, index, 24);
        if SENSITIVE_KEYS.iter().any(|key| window.contains(key)) {
            return true;
        }
    }
    false
}

/// 提取 ASCII 字母数字词（含 `-`、`_`、`.`、`+`），用于令牌前缀判定。
fn ascii_words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = None::<usize>;
    for (index, ch) in text.char_indices() {
        let is_word_char = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+');
        match (start, is_word_char) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                words.push(&text[begin..index]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        words.push(&text[begin..]);
    }
    words
}

/// 提取连续数字串。
fn digit_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// 判断关键词后是否出现"分隔符 + 非空值"的赋值句式。
fn contains_assignment_pattern(text: &str, keys: &[&str], separators: &[&str]) -> bool {
    for key in keys {
        let mut search_from = 0;
        while let Some(relative) = text[search_from..].find(key) {
            let after = search_from + relative + key.len();
            let tail = text[after..].trim_start_matches([' ', '\t', '"', '\'', '`']);
            let has_value = separators.iter().any(|separator| {
                tail.strip_prefix(separator).is_some_and(|rest| {
                    let value = rest.trim_start_matches([' ', '\t', '"', '\'', '`']);
                    !value.is_empty()
                        && value
                            .chars()
                            .take(8)
                            .any(|ch| ch.is_alphanumeric() || matches!(ch, '!' | '@' | '#' | '$'))
                })
            });
            if has_value {
                return true;
            }
            search_from = after;
        }
    }
    false
}

/// 判断关键词后指定窗口内是否出现长度达标的数字串。
fn keyword_followed_by_digit_run(
    text: &str,
    key: &str,
    min_digits: usize,
    window_chars: usize,
) -> bool {
    let mut search_from = 0;
    while let Some(relative) = text[search_from..].find(key) {
        let key_start = search_from + relative;
        let after = key_start + key.len();
        let window = char_window(text, after, window_chars);
        if digit_runs(window).iter().any(|run| run.len() >= min_digits) {
            return true;
        }
        search_from = after;
    }
    false
}

/// 判断关键词后是否跟随达到最小长度的实质内容，避免"什么是 X"类提问误伤。
fn keyword_with_meaningful_tail(text: &str, key: &str, min_tail_chars: usize) -> bool {
    let mut search_from = 0;
    while let Some(relative) = text[search_from..].find(key) {
        let after = search_from + relative + key.len();
        let tail_chars = text[after..]
            .chars()
            .filter(|ch| {
                !ch.is_whitespace()
                    && !matches!(ch, '。' | '，' | ',' | '？' | '?' | '：' | ':' | '、')
            })
            .count();
        if tail_chars >= min_tail_chars {
            return true;
        }
        search_from = after;
    }
    false
}

/// 判断文本是否包含高精度坐标对（两个带四位以上小数的十进制数）。
fn contains_precise_coordinates(text: &str) -> bool {
    let mut precise_numbers = 0_usize;
    for word in ascii_words(text) {
        let candidate = word.trim_start_matches('+').trim_start_matches('-');
        if let Some((integer, fraction)) = candidate.split_once('.')
            && !integer.is_empty()
            && integer.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.len() >= 4
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            precise_numbers += 1;
            if precise_numbers >= 2 {
                return true;
            }
        }
    }
    false
}

/// 判断文本是否包含邮箱形态（local@domain.tld）。
fn contains_email_address(text: &str) -> bool {
    for (index, ch) in text.char_indices() {
        if ch != '@' {
            continue;
        }
        let has_local = text[..index]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
        if !has_local {
            continue;
        }
        let domain: String = text[index + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            .collect();
        if let Some((_, tld)) = domain.rsplit_once('.')
            && tld.len() >= 2
            && tld.bytes().all(|byte| byte.is_ascii_alphabetic())
            && domain.len() > tld.len() + 1
        {
            return true;
        }
    }
    false
}

/// 判断文本是否出现 18 岁以下的年龄表述（"N 岁" / "N 周岁"，允许数字两侧有空白）。
fn age_below_eighteen(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if *ch != '岁' {
            continue;
        }
        let mut digits = String::new();
        for previous in chars[..index].iter().rev().take(4) {
            if previous.is_ascii_digit() {
                digits.insert(0, *previous);
            } else if previous.is_whitespace() && digits.is_empty() {
                continue;
            } else {
                break;
            }
        }
        if !digits.is_empty() && digits.parse::<u32>().is_ok_and(|age| age < 18) {
            return true;
        }
    }
    false
}

/// 中国大陆居民身份证校验位（GB 11643）。
fn chinese_id_checksum_valid(run: &str) -> bool {
    let chars: Vec<char> = run.chars().collect();
    if chars.len() != 18 || !chars[..17].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECK_CODES: [char; 11] = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let sum: u32 = chars[..17]
        .iter()
        .zip(WEIGHTS.iter())
        .map(|(digit, weight)| digit.to_digit(10).unwrap_or(0) * weight)
        .sum();
    let expected = CHECK_CODES[(sum % 11) as usize];
    chars[17].to_ascii_uppercase() == expected
}

/// Luhn 校验，用于银行卡号形态判定。
fn luhn_valid(run: &str) -> bool {
    if !run.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0_u32;
    for (index, byte) in run.bytes().rev().enumerate() {
        let mut digit = u32::from(byte - b'0');
        if index % 2 == 1 {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
    }
    sum.is_multiple_of(10)
}

/// 从指定字节位置取不超过 max_chars 个字符的窗口。
fn char_window(text: &str, byte_start: usize, max_chars: usize) -> &str {
    let mut end = text.len();
    for (count, (index, ch)) in text[byte_start..].char_indices().enumerate() {
        if count == max_chars {
            end = byte_start + index;
            break;
        }
        end = byte_start + index + ch.len_utf8();
    }
    &text[byte_start..end.min(text.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::memory::{
        MemoryCategory, MemoryChangeType, MemoryId, MemoryRevisionId, MemorySafetyStage,
        MemorySourceEvidence, MemorySourceKind,
    };

    fn assess_content(content: &str) -> MemorySafetyAssessment {
        assess_content_at(content, MemorySafetyStage::TurnStaging)
    }

    fn assess_content_at(content: &str, stage: MemorySafetyStage) -> MemorySafetyAssessment {
        let source = MemorySourceEvidence::ConversationTurn {
            conversation_id: "conv-test".to_string(),
            turn_id: "turn-test".to_string(),
            kind: MemorySourceKind::DirectUserMessage,
        };
        let assigned_memory_id = MemoryId("mem-test".to_string());
        let assigned_revision_id = MemoryRevisionId("rev-test".to_string());
        let request = MemorySensitivityRequest {
            stage,
            operation_id: "op-test",
            operation: MemoryChangeType::Create,
            memory_id: None,
            expected_revision_id: None,
            assigned_memory_id: &assigned_memory_id,
            assigned_revision_id: &assigned_revision_id,
            category: MemoryCategory::UserFact,
            importance: None,
            content,
            change_reason: "用户在本轮消息中说明",
            event_time: None,
            source: &source,
        };
        DeterministicMemorySensitivityPolicy::new().assess(request)
    }

    fn assert_rejected(content: &str) {
        assert!(
            matches!(
                assess_content(content),
                MemorySafetyAssessment::Rejected { .. }
            ),
            "应拒绝敏感内容：{content}"
        );
    }

    fn assert_allowed(content: &str) {
        assert!(
            matches!(
                assess_content(content),
                MemorySafetyAssessment::Allowed { .. }
            ),
            "不应误伤普通内容：{content}"
        );
    }

    #[test]
    fn hard_secret_patterns_are_rejected() {
        assert_rejected("我的密钥是 sk-ant-abcdefghijklmnopqrstuvwxyz123456");
        assert_rejected("记下 AKIAIOSFODNN7EXAMPLE 这个访问标识");
        assert_rejected("github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz_0123456789abcdef");
        assert_rejected(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nxxxx\n-----END OPENSSH PRIVATE KEY-----",
        );
        assert_rejected("配置 api_key = abcdefghijklmnopqrstuvwxyz");
        assert_rejected("把 password: hunter2hunter2 记住");
        assert_rejected("我的密码：abc123xyz 很久没改了");
        assert_rejected("令牌=abcdef1234567890 记得保存");
        assert_rejected("authorization: bearer abcdefghijklmnopqrstuvwxyz");
        assert_rejected("验证码：839201，十分钟后失效");
        assert_rejected("password = !hunter2hunter2");
    }

    #[test]
    fn identity_document_patterns_are_rejected() {
        // 校验位合法的测试身份证号（11010519491231002X 是公开的国标示例）。
        assert_rejected("用户身份证号 11010519491231002X 需要记住");
        assert_rejected("他的护照号是 E12345678，别外传");
        assert_rejected("我的身份证 11010519491231002X 前面提过");
    }

    #[test]
    fn financial_payment_patterns_are_rejected() {
        // 6222020200112230 是通过 Luhn 校验的测试卡号形态。
        assert_rejected("工资卡 6222020200112230 每个月 10 号到账");
        assert_rejected("银行卡号：62220202 开头的那张");
        assert_rejected("cvv 是 123，卡先记着");
    }

    #[test]
    fn precise_location_patterns_are_rejected() {
        assert_rejected("我在 39.90420, 116.40740 附近等");
        assert_rejected("家庭住址：朝阳区幸福里小区 3 号楼 2 单元 501");
        assert_rejected("实时位置分享给你了，在北门");
    }

    #[test]
    fn private_contact_patterns_are_rejected() {
        assert_rejected("我的手机号 13812345678 没变");
        assert_rejected("邮箱 some.one+test@example.com 用来收通知");
        assert_rejected("电话 +8613812345678 可以联系到我");
        assert_rejected("微信号 abc12345 加一下");
    }

    #[test]
    fn health_medical_patterns_are_rejected() {
        assert_rejected("上周体检报告出来了");
        assert_rejected("确诊之后一直在调整用药");
        assert_rejected("她有高血压，需要定期复查");
    }

    #[test]
    fn sexual_intimacy_patterns_are_rejected() {
        assert_rejected("我们聊到了性生活的话题");
        assert_rejected("这段亲密行为细节不要记录");
    }

    #[test]
    fn minor_patterns_are_rejected() {
        assert_rejected("儿子今年 7 岁，刚上小学");
        assert_rejected("我是未成年人，别告诉家长");
        assert_rejected("妹妹读初中生二年级");
    }

    #[test]
    fn third_party_privacy_patterns_are_rejected() {
        assert_rejected("我同事的身份证号 110105 开头");
        assert_rejected("妈妈的病历在我这里保管");
        assert_rejected("朋友的手机号是 138 开头的");
    }

    #[test]
    fn ordinary_memories_are_allowed() {
        assert_allowed("用户更喜欢在晚上吃第一顿饭");
        assert_allowed("用户正在学习 Rust，偏好简短直接的回答");
        assert_allowed("我们约定每周五晚上一起回顾项目进度");
        assert_allowed("用户 18 岁生日刚过完，喜欢推理小说");
        assert_allowed("剧情里角色在城北开了一家书店");
        assert_allowed("用户想知道什么是门牌号，以及密码管理器怎么选");
    }

    #[test]
    fn boundary_cases_avoid_false_positives() {
        // "密码" 不带赋值句式不应命中。
        assert_allowed("用户总是忘记密码，想要一个记忆方法");
        // 11 位号码但不是手机号段。
        assert_allowed("订单号 10020030040 需要明天核对");
        // 年龄满 18 不命中未成年规则。
        assert_allowed("用户今年 18 岁，刚参加完高考");
        // 关键词后没有实质尾文（纯提问）不命中住址规则。
        assert_allowed("用户随口问了句「什么是经纬度」");
    }

    #[test]
    fn empty_content_fails_closed() {
        assert!(matches!(
            assess_content("   "),
            MemorySafetyAssessment::FailClosed {
                stage: MemorySafetyStage::TurnStaging,
                reason: MemorySafetyFailure::DecisionMissing,
            }
        ));
    }

    #[test]
    fn empty_change_reason_fails_closed() {
        let source = MemorySourceEvidence::ConversationTurn {
            conversation_id: "conv-test".to_string(),
            turn_id: "turn-test".to_string(),
            kind: MemorySourceKind::DirectUserMessage,
        };
        let assigned_memory_id = MemoryId("mem-test".to_string());
        let assigned_revision_id = MemoryRevisionId("rev-test".to_string());
        let assessment =
            DeterministicMemorySensitivityPolicy::new().assess(MemorySensitivityRequest {
                stage: MemorySafetyStage::TurnStaging,
                operation_id: "op-test",
                operation: MemoryChangeType::Create,
                memory_id: None,
                expected_revision_id: None,
                assigned_memory_id: &assigned_memory_id,
                assigned_revision_id: &assigned_revision_id,
                category: MemoryCategory::UserFact,
                importance: None,
                content: "用户喜欢夜间散步",
                change_reason: "  ",
                event_time: None,
                source: &source,
            });
        assert!(matches!(
            assessment,
            MemorySafetyAssessment::FailClosed {
                reason: MemorySafetyFailure::DecisionMissing,
                ..
            }
        ));
    }

    #[test]
    fn assessment_carries_stage_and_policy_version() {
        let staged = assess_content_at("用户喜欢夜间散步", MemorySafetyStage::TurnStaging);
        assert!(staged.permits_staging());
        assert!(!staged.permits_persistence());

        let commit = assess_content_at("用户喜欢夜间散步", MemorySafetyStage::RepositoryCommit);
        assert!(commit.permits_persistence());
        assert!(!commit.permits_staging());
    }

    #[test]
    fn change_reason_is_scanned_together_with_content() {
        // 命中出现在 change_reason 中的敏感关键词同样拒绝。
        let source = MemorySourceEvidence::ConversationTurn {
            conversation_id: "conv-test".to_string(),
            turn_id: "turn-test".to_string(),
            kind: MemorySourceKind::DirectUserMessage,
        };
        let assigned_memory_id = MemoryId("mem-test".to_string());
        let assigned_revision_id = MemoryRevisionId("rev-test".to_string());
        let request = MemorySensitivityRequest {
            stage: MemorySafetyStage::TurnStaging,
            operation_id: "op-test",
            operation: MemoryChangeType::Create,
            memory_id: None,
            expected_revision_id: None,
            assigned_memory_id: &assigned_memory_id,
            assigned_revision_id: &assigned_revision_id,
            category: MemoryCategory::UserFact,
            importance: None,
            content: "用户提到一件私事",
            change_reason: "用户把确诊结果告诉了角色",
            event_time: None,
            source: &source,
        };
        let assessment = DeterministicMemorySensitivityPolicy::new().assess(request);
        assert!(matches!(
            assessment,
            MemorySafetyAssessment::Rejected { .. }
        ));
    }
}
