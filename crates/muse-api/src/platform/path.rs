//! 处理桌面端常见用户目录和自然语言路径别名展开。

use std::env;
use std::path::{Path, PathBuf};

/// 本机路径上下文。这里只处理跨平台路径语义，不表达任何授权策略。
pub(crate) struct KnownUserDirectory {
    pub label: &'static str,
    pub aliases: &'static [&'static str],
    pub path: PathBuf,
}

/// 获取用户主目录。按桌面应用常见环境变量顺序推导，避免业务层关心操作系统。
pub(crate) fn home_dir() -> Option<PathBuf> {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(path) = env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Some(path);
        }
    }

    let drive = env::var_os("HOMEDRIVE")?;
    let home_path = env::var_os("HOMEPATH")?;
    let mut combined = drive;
    combined.push(home_path);
    let path = PathBuf::from(combined);
    path.is_absolute().then_some(path)
}

/// 展开用户输入路径。支持 `~`、`~/`、`~\`，其他相对路径按给定基准目录解析。
pub(crate) fn expand_user_path(raw: &str, base: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    if let Some(rest) = raw.strip_prefix("~\\")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    if let Some(path) = expand_known_user_directory_alias(raw) {
        return path;
    }

    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn split_first_path_segment(raw: &str) -> (&str, Option<&str>) {
    let slash = raw.find('/');
    let backslash = raw.find('\\');
    let split_at = match (slash, backslash) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };
    match split_at {
        Some(index) => (&raw[..index], Some(&raw[index + 1..])),
        None => (raw, None),
    }
}

fn expand_known_user_directory_alias(raw: &str) -> Option<PathBuf> {
    let (head, rest) = split_first_path_segment(raw);
    if head.is_empty() || head == "." || head == ".." {
        return None;
    }
    known_user_directories()
        .into_iter()
        .find(|directory| {
            directory
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(head))
        })
        .map(|directory| match rest {
            Some(rest) if !rest.is_empty() => directory.path.join(rest),
            _ => directory.path,
        })
}

/// 返回跨平台常见用户目录候选。它们只是路径推导提示，不自动加入允许目录。
pub(crate) fn known_user_directories() -> Vec<KnownUserDirectory> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };

    [
        (
            "下载文件夹",
            &["下载", "下载文件夹", "Downloads", "download"][..],
            "Downloads",
        ),
        ("桌面", &["桌面", "Desktop"][..], "Desktop"),
        ("文稿", &["文稿", "文档", "Documents"][..], "Documents"),
        ("图片", &["图片", "Pictures"][..], "Pictures"),
        ("音乐", &["音乐", "Music"][..], "Music"),
        ("视频", &["视频", "Movies", "Videos"][..], "Videos"),
    ]
    .into_iter()
    .map(|(label, aliases, directory)| KnownUserDirectory {
        label,
        aliases,
        path: home.join(directory),
    })
    .collect()
}

/// 返回已知用户目录提示词，供模型做自然语言路径定位。
pub(crate) fn known_user_directories_prompt() -> String {
    let directories = known_user_directories();
    if directories.is_empty() {
        return "常见用户目录候选：未知（未能识别用户主目录）。".to_string();
    }

    let lines = directories
        .into_iter()
        .map(|directory| {
            format!(
                "- {}（{}）：{}",
                directory.label,
                directory.aliases.join(" / "),
                directory.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("常见用户目录候选（仅用于路径推导，不代表已授权）：\n{lines}")
}
