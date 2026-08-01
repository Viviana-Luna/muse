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
use unicode_normalization::UnicodeNormalization;

/// 当前确定性规则集版本；规则变化必须同步升级，持久化 revision 凭它审计。
pub const MEMORY_SENSITIVITY_POLICY_VERSION: &str = "deterministic-nfkc-v3";

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
        let content = match NormalizedSensitiveText::new(request.content) {
            Ok(content) => content,
            Err(reason) => {
                return MemorySafetyAssessment::FailClosed {
                    stage: request.stage,
                    reason,
                };
            }
        };
        let change_reason = match NormalizedSensitiveText::new(request.change_reason) {
            Ok(change_reason) => change_reason,
            Err(reason) => {
                return MemorySafetyAssessment::FailClosed {
                    stage: request.stage,
                    reason,
                };
            }
        };
        // 两道门都由本实现先做同一版 NFKC、大小写、零宽字符、同形字符和
        // 分隔符规范化，再运行完全相同的规则；任一字段命中即整项拒绝。
        let rejected = hits_sensitive_rule(&content).is_some()
            || hits_sensitive_rule(&change_reason).is_some();
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

#[derive(Debug)]
struct NormalizedSensitiveText {
    canonical: String,
    compact: String,
    json_fields: Vec<(String, String)>,
}

impl NormalizedSensitiveText {
    fn new(raw: &str) -> Result<Self, MemorySafetyFailure> {
        let canonical = normalize_sensitive_scalar(raw)?;
        if canonical.trim().is_empty() {
            return Err(MemorySafetyFailure::DecisionMissing);
        }
        let compact = canonical
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>();
        if compact.is_empty() {
            return Err(MemorySafetyFailure::Indeterminate);
        }

        let trimmed = raw.trim();
        let mut json_fields = Vec::new();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            let parsed = serde_json::from_str::<serde_json::Value>(trimmed)
                .map_err(|_| MemorySafetyFailure::Indeterminate)?;
            collect_json_fields(&parsed, &mut json_fields, 0)?;
        }
        Ok(Self {
            canonical,
            compact,
            json_fields,
        })
    }
}

type SensitiveRule = fn(&NormalizedSensitiveText) -> bool;

fn hits_sensitive_rule(text: &NormalizedSensitiveText) -> Option<SensitiveCategory> {
    let rules: [(SensitiveCategory, SensitiveRule); 9] = [
        (SensitiveCategory::HardSecret, |text| {
            contains_hard_secret(text)
        }),
        (SensitiveCategory::IdentityDocument, |text| {
            contains_identity_document(&text.compact)
        }),
        (SensitiveCategory::FinancialPayment, |text| {
            contains_financial_payment(&text.compact)
        }),
        (SensitiveCategory::PreciseLocation, |text| {
            contains_precise_location(&text.canonical, &text.compact)
        }),
        (SensitiveCategory::PrivateContact, |text| {
            contains_private_contact(&text.canonical, &text.compact)
        }),
        (SensitiveCategory::HealthMedical, |text| {
            contains_health_medical(&text.compact)
        }),
        (SensitiveCategory::SexualIntimacy, |text| {
            contains_sexual_intimacy(&text.compact)
        }),
        (SensitiveCategory::Minor, |text| {
            contains_minor_reference(&text.canonical, &text.compact)
        }),
        (SensitiveCategory::ThirdPartyPrivacy, |text| {
            contains_third_party_privacy(&text.compact)
        }),
    ];
    rules
        .iter()
        .find(|(_, rule)| contains_sensitive_field_assignment(text) || rule(text))
        .map(|(category, _)| *category)
}

fn normalize_sensitive_scalar(value: &str) -> Result<String, MemorySafetyFailure> {
    let mut normalized = String::new();
    let mut previous_space = false;
    for character in value.nfkc().flat_map(char::to_lowercase) {
        if is_ignored_format_character(character) {
            continue;
        }
        if character.is_control() {
            if matches!(character, '\n' | '\r' | '\t') {
                push_normalized_space(&mut normalized, &mut previous_space);
                continue;
            }
            return Err(MemorySafetyFailure::Indeterminate);
        }
        let character = fold_common_homoglyph(character);
        if character.is_alphanumeric() || matches!(character, '@' | '+' | '.' | '-' | '_') {
            normalized.push(character);
            previous_space = false;
        } else if is_assignment_separator(character) {
            while normalized.ends_with(' ') {
                normalized.pop();
            }
            if !normalized.ends_with(':') {
                normalized.push(':');
            }
            previous_space = false;
        } else {
            push_normalized_space(&mut normalized, &mut previous_space);
        }
    }
    Ok(normalized.trim().to_string())
}

fn push_normalized_space(normalized: &mut String, previous_space: &mut bool) {
    if !*previous_space && !normalized.is_empty() && !normalized.ends_with(':') {
        normalized.push(' ');
    }
    *previous_space = true;
}

fn is_assignment_separator(character: char) -> bool {
    matches!(
        character,
        ':' | '=' | '/' | '\\' | '|' | '→' | '⇒' | '➜' | '⟶' | '⟹' | '﹕' | '︰'
    )
}

fn is_ignored_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

fn fold_common_homoglyph(character: char) -> char {
    match character {
        'а' | 'α' => 'a',
        'в' | 'β' => 'b',
        'с' | 'ϲ' => 'c',
        'ԁ' => 'd',
        'е' | 'ε' => 'e',
        'һ' | 'η' => 'h',
        'і' | 'ι' => 'i',
        'ј' => 'j',
        'κ' => 'k',
        'м' | 'μ' => 'm',
        'ո' => 'n',
        'о' | 'ο' => 'o',
        'р' | 'ρ' => 'p',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'ԝ' => 'w',
        'х' | 'χ' => 'x',
        'у' | 'υ' => 'y',
        _ => character,
    }
}

fn collect_json_fields(
    value: &serde_json::Value,
    fields: &mut Vec<(String, String)>,
    depth: usize,
) -> Result<(), MemorySafetyFailure> {
    if depth > 32 || fields.len() > 256 {
        return Err(MemorySafetyFailure::Indeterminate);
    }
    match value {
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let key = normalize_sensitive_scalar(key)?;
                match value {
                    serde_json::Value::String(value) => {
                        push_json_field(fields, key, normalize_sensitive_scalar(value)?)?;
                    }
                    serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                        push_json_field(fields, key, value.to_string())?;
                    }
                    serde_json::Value::Null => push_json_field(fields, key, String::new())?,
                    serde_json::Value::Object(values) => {
                        if !values.is_empty() {
                            push_json_field(fields, key, "nested".to_string())?;
                        }
                        collect_json_fields(value, fields, depth + 1)?;
                    }
                    serde_json::Value::Array(values) => {
                        if !values.is_empty() {
                            push_json_field(fields, key, "nested".to_string())?;
                        }
                        collect_json_fields(value, fields, depth + 1)?;
                    }
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_json_fields(value, fields, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn push_json_field(
    fields: &mut Vec<(String, String)>,
    key: String,
    value: String,
) -> Result<(), MemorySafetyFailure> {
    if fields.len() >= 256 {
        return Err(MemorySafetyFailure::Indeterminate);
    }
    fields.push((key, value));
    Ok(())
}

fn contains_sensitive_field_assignment(text: &NormalizedSensitiveText) -> bool {
    text.json_fields
        .iter()
        .any(|(key, value)| sensitive_field_name(key) && has_field_value(value))
        || text
            .canonical
            .split(':')
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| sensitive_field_name(pair[0]) && has_field_value(pair[1]))
}

fn sensitive_field_name(value: &str) -> bool {
    let identifier = value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    const FIELD_NAMES: [&str; 48] = [
        "pwd",
        "pass",
        "password",
        "passwd",
        "passcode",
        "token",
        "credential",
        "secret",
        "apikey",
        "secretkey",
        "clientsecret",
        "accesstoken",
        "refreshtoken",
        "idtoken",
        "privatekey",
        "authtoken",
        "sessiontoken",
        "signingkey",
        "bearertoken",
        "otp",
        "pin",
        "phone",
        "phonenumber",
        "mobile",
        "mobilephone",
        "idcard",
        "identitynumber",
        "passportnumber",
        "bankcard",
        "cardnumber",
        "homeaddress",
        "streetaddress",
        "preciseaddress",
        "address",
        "密码",
        "口令",
        "密钥",
        "凭据",
        "令牌",
        "验证码",
        "支付码",
        "手机号",
        "电话号码",
        "身份证",
        "护照号",
        "银行卡号",
        "详细住址",
        "家庭住址",
    ];
    FIELD_NAMES.iter().any(|field| identifier == *field)
}

fn has_field_value(value: &str) -> bool {
    value.chars().any(|character| character.is_alphanumeric())
}

/// 硬秘密与认证凭据：PEM 私钥块、已知令牌前缀、密钥赋值句式。
fn contains_hard_secret(text: &NormalizedSensitiveText) -> bool {
    let canonical = text.canonical.as_str();
    if text.compact.contains("begin") && text.compact.contains("privatekey") {
        return true;
    }
    const TOKEN_PREFIXES: [&str; 24] = [
        "sk-ant-",
        "sk-proj-",
        "sk_live_",
        "sk_test_",
        "sk-",
        "rk_live_",
        "akia",
        "asia",
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
        "aiza",
        "dop_v1_",
        "hf_",
    ];
    const COMPACT_TOKEN_PREFIXES: [&str; 12] = [
        "skant",
        "skproj",
        "sklive",
        "sktest",
        "githubpat",
        "glpat",
        "xoxb",
        "xoxp",
        "ya29",
        "aiza",
        "dopv1",
        "hf",
    ];
    if COMPACT_TOKEN_PREFIXES.iter().any(|prefix| {
        text.compact
            .find(prefix)
            .is_some_and(|index| text.compact[index..].len() >= prefix.len() + 16)
    }) {
        return true;
    }
    for word in ascii_words(canonical) {
        for prefix in TOKEN_PREFIXES {
            if word.starts_with(prefix) && word.len() >= prefix.len() + 16 {
                return true;
            }
        }
        // JWT 固定以 eyJ 开头且含两段分隔；短词不作为凭据。
        if word.starts_with("eyj") && word.len() >= 32 && word.matches('.').count() >= 2 {
            return true;
        }
    }
    if text.compact.contains("authorizationbearer")
        || text.compact.contains("authorizationbasic")
        || text.compact.contains("cookiesession")
        || text.compact.contains("setcookiesession")
    {
        return true;
    }
    const ASSIGNMENT_KEYS: [&str; 22] = [
        "pwd",
        "pass",
        "password",
        "passwd",
        "token",
        "credential",
        "secret",
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
    if contains_assignment_pattern(canonical, &ASSIGNMENT_KEYS, &[":"])
        || contains_spaced_hyphen_assignment(canonical, &ASSIGNMENT_KEYS)
    {
        return true;
    }
    const CJK_CREDENTIAL_KEYS: [&str; 7] =
        ["密码", "口令", "密钥", "凭据", "令牌", "验证码", "支付码"];
    contains_assignment_pattern(canonical, &CJK_CREDENTIAL_KEYS, &[":"])
        || contains_spaced_hyphen_assignment(canonical, &CJK_CREDENTIAL_KEYS)
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
fn contains_financial_payment(text: &str) -> bool {
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
        .any(|key| keyword_followed_by_digit_run(text, key, 3, 24))
}

/// 精确住址与实时位置：高精度坐标对、结构化门牌地址，或明确的住址/定位字段。
fn contains_precise_location(canonical: &str, compact: &str) -> bool {
    if contains_precise_coordinates(canonical) {
        return true;
    }
    if keyword_followed_by_digit_run(compact, "门牌号", 1, 16) {
        return true;
    }
    if contains_street_house_number(canonical, compact) {
        return true;
    }
    const LOCATION_KEYS: [&str; 8] = [
        "详细住址",
        "家庭住址",
        "居住地址",
        "收货地址",
        "实时位置",
        "定位到",
        "经纬度",
        "gps坐标",
    ];
    LOCATION_KEYS
        .iter()
        .any(|key| keyword_with_meaningful_tail(compact, key, 4))
}

/// 私人联系方式：手机号、国际号码、邮箱地址，或联系方式关键词伴随号码。
fn contains_private_contact(canonical: &str, compact: &str) -> bool {
    if contains_phone_number(canonical) {
        return true;
    }
    for run in digit_runs(compact) {
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
    if contains_email_address(canonical) {
        return true;
    }
    const CONTACT_KEYS: [&str; 5] = ["微信号", "QQ号", "qq号", "手机号", "电话号码"];
    CONTACT_KEYS
        .iter()
        .any(|key| keyword_followed_by_digit_run(compact, key, 5, 16))
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
fn contains_minor_reference(_canonical: &str, compact: &str) -> bool {
    const MINOR_KEYS: [&str; 7] = [
        "未成年",
        "未满18",
        "未满十八",
        "小学生",
        "初中生",
        "适龄儿童",
        "幼儿园",
    ];
    if MINOR_KEYS.iter().any(|key| compact.contains(key)) {
        return true;
    }
    age_below_eighteen(compact)
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
            let key_start = search_from + relative;
            let after = key_start + key.len();
            if !has_assignment_key_boundary(text, key, key_start) {
                search_from = after;
                continue;
            }
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

/// 连字符只有在键值之间至少一侧保留空白时才视作赋值符，避免把
/// `password-free` 之类普通复合词误判为凭据。
fn contains_spaced_hyphen_assignment(text: &str, keys: &[&str]) -> bool {
    for key in keys {
        let mut search_from = 0;
        while let Some(relative) = text[search_from..].find(key) {
            let key_start = search_from + relative;
            let after = key_start + key.len();
            if !has_assignment_key_boundary(text, key, key_start) {
                search_from = after;
                continue;
            }
            let tail = &text[after..];
            let Some(hyphen_index) = tail.find('-') else {
                search_from = after;
                continue;
            };
            if !tail[..hyphen_index].chars().all(char::is_whitespace) {
                search_from = after;
                continue;
            }
            let value = &tail[hyphen_index + 1..];
            let has_spacing =
                hyphen_index > 0 || value.chars().next().is_some_and(char::is_whitespace);
            if has_spacing
                && value
                    .trim_start()
                    .chars()
                    .take(8)
                    .any(|ch| ch.is_alphanumeric() || matches!(ch, '!' | '@' | '#' | '$'))
            {
                return true;
            }
            search_from = after;
        }
    }
    false
}

fn has_assignment_key_boundary(text: &str, key: &str, key_start: usize) -> bool {
    !key.is_ascii()
        || key_start == 0
        || text[..key_start]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
}

/// 识别结构化中文门牌地址，或英文 Street/Road 等标记附近的门牌数字。
///
/// 中文规则只接受“道路/住宅标记紧邻编号与门牌单位”，或“至少两级位置链之后
/// 出现编号与门牌单位”。数字必须与 `号`、`号楼`、`栋`、`幢`、`单元`、`室`
/// 直接组成结构，不能在道路词之后开一个任意字符窗口寻找数字。
fn contains_street_house_number(canonical: &str, compact: &str) -> bool {
    if contains_structured_cjk_address(compact) {
        return true;
    }

    const ASCII_STREET_MARKERS: [&str; 6] =
        ["street", "road", "avenue", "lane", "drive", "boulevard"];
    let words = ascii_words(canonical);
    for (index, word) in words.iter().enumerate() {
        let marker = word.trim_matches(['.', '-']);
        if !ASCII_STREET_MARKERS.contains(&marker) {
            continue;
        }
        let start = index.saturating_sub(3);
        let end = (index + 4).min(words.len());
        if words[start..end].iter().any(|candidate| {
            let candidate = candidate.trim_matches(['.', '-']);
            !candidate.is_empty() && candidate.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return true;
        }
    }
    false
}

fn contains_structured_cjk_address(text: &str) -> bool {
    const ADDRESS_MARKERS: [&str; 20] = [
        "住宅小区",
        "商业大厦",
        "工业园区",
        "科技园区",
        "大道",
        "胡同",
        "公路",
        "街道",
        "小区",
        "社区",
        "花园",
        "家园",
        "大厦",
        "公寓",
        "园区",
        "里弄",
        "路",
        "街",
        "巷",
        "弄",
    ];

    for number_start in cjk_address_number_starts(text) {
        let number_end = text[number_start..]
            .char_indices()
            .take_while(|(_, character)| is_cjk_address_number(*character))
            .last()
            .map(|(index, character)| number_start + index + character.len_utf8())
            .unwrap_or(number_start);
        if number_end == number_start || !starts_with_cjk_house_unit(&text[number_end..]) {
            continue;
        }

        let prefix = &text[..number_start];
        if ADDRESS_MARKERS
            .iter()
            .any(|marker| prefix.ends_with(marker))
            || contains_cjk_location_chain(prefix)
        {
            return true;
        }
    }
    false
}

fn cjk_address_number_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous_was_number = false;
    for (index, character) in text.char_indices() {
        let is_number = is_cjk_address_number(character);
        if is_number && !previous_was_number {
            starts.push(index);
        }
        previous_was_number = is_number;
    }
    starts
}

fn is_cjk_address_number(character: char) -> bool {
    character.is_ascii_digit()
        || matches!(
            character,
            '零' | '〇'
                | '一'
                | '二'
                | '两'
                | '三'
                | '四'
                | '五'
                | '六'
                | '七'
                | '八'
                | '九'
                | '十'
                | '百'
                | '千'
                | '万'
                | '壹'
                | '贰'
                | '叁'
                | '肆'
                | '伍'
                | '陆'
                | '柒'
                | '捌'
                | '玖'
                | '拾'
                | '佰'
                | '仟'
        )
}

fn starts_with_cjk_house_unit(value: &str) -> bool {
    const HOUSE_UNITS: [&str; 6] = ["号楼", "单元", "号", "栋", "幢", "室"];
    HOUSE_UNITS.iter().any(|unit| value.starts_with(unit))
}

fn contains_cjk_location_chain(prefix: &str) -> bool {
    const LOCATION_COMPONENTS: [&str; 12] = [
        "自治区",
        "自治州",
        "特别行政区",
        "街道",
        "省",
        "市",
        "区",
        "县",
        "旗",
        "镇",
        "乡",
        "村",
    ];
    let mut components = 0_usize;
    let mut cursor = 0_usize;
    while cursor < prefix.len() {
        let tail = &prefix[cursor..];
        if let Some(component) = LOCATION_COMPONENTS
            .iter()
            .find(|component| tail.starts_with(**component))
        {
            components += 1;
            if components >= 2 {
                return true;
            }
            cursor += component.len();
        } else {
            cursor += tail.chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }
    false
}

/// 识别允许括号、空格和横线分隔的手机号；带 `+86` 时先压缩国家码。
fn contains_phone_number(canonical: &str) -> bool {
    let chars = canonical.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '+' && !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let started_with_plus = chars[index] == '+';
        let mut digits = String::new();
        let mut cursor = index;
        while cursor < chars.len() {
            let character = chars[cursor];
            if character.is_ascii_digit() {
                digits.push(character);
            } else if matches!(character, '+' | '-' | ' ' | '(' | ')') {
                // 电话内部常见分隔符；字母和其他标点会结束候选。
            } else {
                break;
            }
            cursor += 1;
        }
        let local = if started_with_plus {
            digits.strip_prefix("86").unwrap_or(&digits)
        } else {
            digits.as_str()
        };
        if local.len() == 11
            && local.starts_with('1')
            && local
                .chars()
                .nth(1)
                .is_some_and(|character| ('3'..='9').contains(&character))
        {
            return true;
        }
        if started_with_plus && (8..=15).contains(&digits.len()) {
            return true;
        }
        index = cursor.max(index + 1);
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
        assert_rejected("我家在幸福路 88 号");
        assert_rejected("我住在北京市朝阳区幸福小区3号楼");
        assert_rejected("我住在北京市朝阳区幸福小区三号楼二单元五〇一室");
        assert_rejected("我住在幸福路八十八号");
        assert_rejected("我住在幸福花园6栋1203室");
        assert_rejected("收件地点是 123 Main Street");
    }

    #[test]
    fn private_contact_patterns_are_rejected() {
        assert_rejected("我的手机号 13812345678 没变");
        assert_rejected("邮箱 some.one+test@example.com 用来收通知");
        assert_rejected("电话 +8613812345678 可以联系到我");
        assert_rejected("电话 +86 (138) 1234-5678 可以联系到我");
        assert_rejected("手机号 138 1234 5678 没变");
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
        // 安全工具名与带连字符的普通形容词不是地址或凭据赋值。
        assert_allowed("用户正在学习 AddressSanitizer 的使用方法");
        assert_allowed("用户偏好 password-free 登录体验");
        assert_allowed("用户用 compass: north 描述导航方向");
        assert_allowed("用户正在阅读 Main Street. 这本小说");
        assert_allowed("我喜欢公路自行车，计划周末骑20公里");
        assert_allowed("用户选择技术路线2");
        assert_allowed("用户计划完成道路测试3轮");
    }

    #[test]
    fn normalization_blocks_unicode_homoglyphs_and_nested_fields() {
        assert_rejected(
            "ｓｋ－ａｎｔ－ａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ１２３４５６",
        );
        assert_rejected("pаss\u{200b}ｗorԁ：hunter2hunter2");
        assert_rejected("pаssԝorԁ：hunter2hunter2");
        assert_rejected(r#"{"profile":{"p.a.s.s.w.o.r.d":"hunter2hunter2"}}"#);
        assert_rejected("p-а-s-s-w-o-r-d ⇒ hunter2hunter2");
        assert_rejected("password / hunter2hunter2");
        assert_rejected("id-card | masked-abc-123");
        assert_rejected(r#"{"outer":{"credential":{"access_token":"abcdef1234567890"}}}"#);
        for key in [
            "pwd",
            "pass",
            "passwd",
            "password",
            "token",
            "access_token",
            "refresh_token",
            "credential",
            "secret",
            "api_key",
        ] {
            assert_rejected(&format!(
                r#"{{"outer":{{"nested":{{"{key}":"probe-value-123"}}}}}}"#
            ));
        }
        assert_rejected("pwd ⟹ probe-value-123");
        assert_rejected("secret | probe-value-123");
    }

    #[test]
    fn normalization_joins_obfuscated_identity_contact_and_address() {
        assert_rejected("手机号：１３８ １２３４ ５６７８");
        assert_rejected("身份证：110105 19491231 002X");
        assert_rejected("家庭\u{200b}住址：北京市朝阳区幸福路 3 号楼 2 单元 501 室");
    }

    #[test]
    fn malformed_json_like_payload_fails_closed_at_both_gates() {
        for stage in [
            MemorySafetyStage::TurnStaging,
            MemorySafetyStage::RepositoryCommit,
        ] {
            assert!(matches!(
                assess_content_at(r#"{"password":"unterminated""#, stage),
                MemorySafetyAssessment::FailClosed {
                    reason: MemorySafetyFailure::Indeterminate,
                    ..
                }
            ));
        }
    }

    #[test]
    fn both_gates_use_the_exact_same_policy_version() {
        let staged = assess_content_at("用户喜欢夜间散步", MemorySafetyStage::TurnStaging);
        let committed = assess_content_at("用户喜欢夜间散步", MemorySafetyStage::RepositoryCommit);
        let MemorySafetyAssessment::Allowed {
            policy_version: staged_version,
            ..
        } = staged
        else {
            panic!("第一道门应允许普通记忆")
        };
        let MemorySafetyAssessment::Allowed {
            policy_version: committed_version,
            ..
        } = committed
        else {
            panic!("第二道门应允许普通记忆")
        };
        assert_eq!(staged_version, MEMORY_SENSITIVITY_POLICY_VERSION);
        assert_eq!(committed_version, MEMORY_SENSITIVITY_POLICY_VERSION);
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
