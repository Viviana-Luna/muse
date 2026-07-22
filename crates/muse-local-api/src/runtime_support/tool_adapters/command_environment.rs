//! 命令工具子进程的最小环境变量边界。

use std::ffi::{OsStr, OsString};

use tokio::process::Command;

/// 拒绝会把工作负载交给当前进程树之外的服务管理器或新会话的命令。
///
/// Unix 进程组和 Windows Job Object 能可靠回收正常派生的子孙，但无法承诺回收
/// `setsid`、系统服务管理器或计划任务接管后的进程。命令工具对这些已知逃逸入口
/// fail-closed；需要长期服务时应由用户在工具外显式管理。
pub(in crate::runtime_support) fn command_containment_escape_reason(
    command: &str,
) -> Option<&'static str> {
    #[cfg(unix)]
    {
        unix_containment_escape_reason(command)
    }
    #[cfg(windows)]
    {
        windows_containment_escape_reason(command)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        Some("当前平台不支持可验证的命令进程树隔离。")
    }
}

#[cfg(any(unix, test))]
fn unix_containment_escape_reason(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    if [
        "os.setsid(",
        "posix::setsid",
        "process.daemon(",
        "daemon.daemoncontext",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return Some("命令包含创建独立会话或守护进程的代码。");
    }

    for segment in shell_command_segments(&normalized) {
        let executable = first_effective_executable(segment);
        if matches!(
            executable,
            Some(
                "setsid"
                    | "daemon"
                    | "daemonize"
                    | "start-stop-daemon"
                    | "systemd-run"
                    | "launchctl"
                    | "service"
                    | "rc-service"
                    | "crontab"
                    | "at"
                    | "batch"
            )
        ) {
            return Some("命令会把进程交给独立会话、服务管理器或计划任务。");
        }
    }
    None
}

#[cfg(any(windows, test))]
fn windows_containment_escape_reason(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    for segment in shell_command_segments(&normalized) {
        let executable = first_effective_executable(segment);
        if matches!(
            executable,
            Some(
                "schtasks"
                    | "schtasks.exe"
                    | "register-scheduledtask"
                    | "start-job"
                    | "start-service"
                    | "new-service"
            )
        ) {
            return Some("命令会把进程交给服务管理器或计划任务。");
        }
    }
    None
}

#[cfg(any(unix, windows, test))]
fn shell_command_segments(command: &str) -> impl Iterator<Item = &str> {
    command.split(['\n', ';', '|', '&'])
}

#[cfg(any(unix, windows, test))]
fn first_effective_executable(segment: &str) -> Option<&str> {
    let mut tokens = segment
        .split_whitespace()
        .map(|token| token.trim_matches(['(', ')', '{', '}', '\'', '"']))
        .filter(|token| !token.is_empty());
    loop {
        let token = tokens.next()?;
        if token.contains('=') && !token.starts_with(['/', '.']) {
            continue;
        }
        if matches!(
            token,
            "then" | "do" | "else" | "exec" | "command" | "env" | "sudo" | "nohup"
        ) {
            continue;
        }
        return token.rsplit(['/', '\\']).next();
    }
}

/// 清空继承环境，只向命令子进程传递运行 shell 所需的最小白名单。
pub(in crate::runtime_support) fn apply_command_environment(process: &mut Command) {
    apply_command_environment_from(process, std::env::vars_os());
}

fn apply_command_environment_from<I>(process: &mut Command, variables: I)
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    process.env_clear();
    process.envs(filter_command_environment(variables));
}

fn filter_command_environment<I>(variables: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    variables
        .into_iter()
        .filter(|(name, _)| is_allowed_environment_name(name))
        .collect()
}

fn is_allowed_environment_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if is_sensitive_environment_name(name) {
        return false;
    }

    #[cfg(windows)]
    {
        is_allowed_windows_environment_name(name)
    }
    #[cfg(unix)]
    {
        is_allowed_unix_environment_name(name)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = name;
        false
    }
}

fn is_sensitive_environment_name(name: &str) -> bool {
    let normalized = name.to_ascii_uppercase();
    const CREDENTIAL_MARKERS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "API_KEY",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "AUTH",
    ];
    const SECRET_BEARING_PREFIXES: &[&str] = &[
        "MUSE_",
        "MCP_",
        "LLM_",
        "OPENAI_",
        "ANTHROPIC_",
        "DEEPSEEK_",
        "GEMINI_",
        "GOOGLE_AI_",
        "AZURE_OPENAI_",
        "COHERE_",
        "MISTRAL_",
        "GROQ_",
        "OPENROUTER_",
        "BRAVE_",
        "TAVILY_",
        "SERPER_",
        "SERPAPI_",
    ];

    CREDENTIAL_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
        || SECRET_BEARING_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

#[cfg(any(unix, test))]
fn is_allowed_unix_environment_name(name: &str) -> bool {
    matches!(
        name,
        "PATH"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "LANG"
            | "LANGUAGE"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "TERM"
            | "COLORTERM"
            | "TZ"
    ) || name.starts_with("LC_")
}

#[cfg(any(windows, test))]
fn is_allowed_windows_environment_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATH"
            | "PATHEXT"
            | "TEMP"
            | "TMP"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "PROGRAMDATA"
            | "PROGRAMFILES"
            | "PROGRAMFILES(X86)"
            | "PROGRAMW6432"
            | "COMMONPROGRAMFILES"
            | "COMMONPROGRAMFILES(X86)"
            | "COMMONPROGRAMW6432"
            | "PSMODULEPATH"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables(names: &[&str]) -> Vec<(OsString, OsString)> {
        names
            .iter()
            .map(|name| (OsString::from(name), OsString::from("测试值")))
            .collect()
    }

    #[test]
    fn pure_filter_keeps_only_current_platform_allowlist() {
        let filtered = filter_command_environment(variables(&[
            "PATH",
            "HOME",
            "MUSE_API_TOKEN",
            "LLM_API_KEY",
            "EXA_API_KEY",
            "DEPLOY_TOKEN",
            "SSH_AUTH_SOCK",
            "AWS_ACCESS_KEY_ID",
            "UNRELATED_SETTING",
        ]));
        let names = filtered
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();

        assert!(names.contains(&OsString::from("PATH")));
        assert!(!names.contains(&OsString::from("MUSE_API_TOKEN")));
        assert!(!names.contains(&OsString::from("LLM_API_KEY")));
        assert!(!names.contains(&OsString::from("EXA_API_KEY")));
        assert!(!names.contains(&OsString::from("DEPLOY_TOKEN")));
        assert!(!names.contains(&OsString::from("SSH_AUTH_SOCK")));
        assert!(!names.contains(&OsString::from("AWS_ACCESS_KEY_ID")));
        assert!(!names.contains(&OsString::from("UNRELATED_SETTING")));
    }

    #[test]
    fn allowlists_cover_required_unix_and_windows_runtime_names() {
        assert!(is_allowed_unix_environment_name("PATH"));
        assert!(is_allowed_unix_environment_name("LC_ALL"));
        assert!(is_allowed_unix_environment_name("TMPDIR"));
        assert!(!is_allowed_unix_environment_name("SSH_AUTH_SOCK"));

        assert!(is_allowed_windows_environment_name("SystemRoot"));
        assert!(is_allowed_windows_environment_name("Path"));
        assert!(is_allowed_windows_environment_name("ProgramFiles(x86)"));
        assert!(is_allowed_windows_environment_name("PSModulePath"));
        assert!(!is_allowed_windows_environment_name("AZURE_ACCESS_TOKEN"));
    }

    #[test]
    fn credential_markers_override_wildcard_runtime_names() {
        assert!(is_allowed_unix_environment_name("LC_API_TOKEN"));
        assert!(is_sensitive_environment_name("LC_API_TOKEN"));
        assert!(!is_allowed_environment_name(OsStr::new("LC_API_TOKEN")));
        assert!(is_sensitive_environment_name("custom_secret"));
        assert!(is_sensitive_environment_name("provider_api_key"));
    }

    #[test]
    fn containment_escape_detection_rejects_detached_launchers_but_not_text_searches() {
        assert!(unix_containment_escape_reason("setsid ./server").is_some());
        assert!(unix_containment_escape_reason("echo ok | sudo setsid ./server").is_some());
        assert!(unix_containment_escape_reason("python -c 'import os; os.setsid()'").is_some());
        assert!(
            unix_containment_escape_reason("launchctl bootstrap gui/501 service.plist").is_some()
        );
        assert!(unix_containment_escape_reason("rg setsid crates/muse-local-api/src").is_none());
        assert!(unix_containment_escape_reason("printf 'setsid is documented'").is_none());

        assert!(windows_containment_escape_reason("schtasks.exe /Create /TN Muse").is_some());
        assert!(windows_containment_escape_reason("Get-Content schtasks.exe.txt").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_child_process_cannot_observe_parent_credentials() {
        let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
        let mut process = Command::new("/bin/sh");
        process.arg("-c").arg(
            r#"printf 'PATH=%s\nMUSE=%s\nLLM=%s\nEXA=%s\nCUSTOM=%s\n' "$PATH" "${MUSE_API_TOKEN-unset}" "${LLM_API_KEY-unset}" "${EXA_API_KEY-unset}" "${CUSTOM_DEPLOY_TOKEN-unset}"; command -v sh"#,
        );
        apply_command_environment_from(
            &mut process,
            vec![
                (OsString::from("PATH"), path),
                (
                    OsString::from("MUSE_API_TOKEN"),
                    OsString::from("muse-secret"),
                ),
                (OsString::from("LLM_API_KEY"), OsString::from("llm-secret")),
                (OsString::from("EXA_API_KEY"), OsString::from("exa-secret")),
                (
                    OsString::from("CUSTOM_DEPLOY_TOKEN"),
                    OsString::from("custom-secret"),
                ),
            ],
        );

        let output = process.output().await.expect("真实 shell 子进程应可启动");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("shell 输出应为 UTF-8");
        assert!(
            stdout
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("PATH=") && line.len() > 5)
        );
        assert!(stdout.contains("MUSE=unset\n"));
        assert!(stdout.contains("LLM=unset\n"));
        assert!(stdout.contains("EXA=unset\n"));
        assert!(stdout.contains("CUSTOM=unset\n"));
        assert!(stdout.lines().any(|line| line.ends_with("/sh")));
    }
}
