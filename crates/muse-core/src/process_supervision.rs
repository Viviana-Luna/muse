//! 跨平台子进程树监管，供命令工具与 MCP stdio transport 共享。

use tokio::process::{Child, Command};

/// 在 spawn 前启用丢弃回收和平台进程树隔离。
pub fn configure_process_tree(command: &mut Command) {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(windows_job::CREATE_SUSPENDED_NO_WINDOW_FLAGS);
}

/// 已启动进程树的同步兜底守卫。
pub struct ProcessTreeGuard {
    child_pid: Option<u32>,
    armed: bool,
    #[cfg(windows)]
    job: windows_job::WindowsProcessJob,
}

impl ProcessTreeGuard {
    /// spawn 后必须立即调用；Windows 会先加入 Job Object，再恢复初始线程。
    pub fn attach(child: &Child) -> Result<Self, String> {
        Ok(Self {
            child_pid: child.id(),
            armed: true,
            #[cfg(windows)]
            job: windows_job::WindowsProcessJob::attach_and_resume(child)?,
        })
    }

    /// 向完整进程树发送终止信号。
    pub fn terminate(&self, force: bool) -> Result<(), String> {
        #[cfg(unix)]
        {
            let Some(pid) = self.child_pid else {
                return Err("未取得子进程 PID，无法终止进程组。".to_string());
            };
            let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
            let result = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("发送进程组终止信号失败：{error}"));
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.job.terminate(if force { 1 } else { 0xC000_013Au32 })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = force;
            Ok(())
        }
    }

    /// 正常退出且已确认进程树没有遗留成员后解除同步兜底。
    pub fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.terminate(true)
        {
            tracing::error!(child_pid = self.child_pid, %error, "丢弃子进程 Future 时清理进程树失败");
        }
        // Windows job 的 Drop 还会关闭 KILL_ON_JOB_CLOSE 句柄，作为内核级最终兜底。
    }
}

#[cfg(windows)]
mod windows_job {
    use std::mem::size_of;
    use tokio::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    pub const CREATE_SUSPENDED_NO_WINDOW_FLAGS: u32 = CREATE_SUSPENDED | CREATE_NO_WINDOW;

    pub struct WindowsProcessJob {
        handle: HANDLE,
    }

    unsafe impl Send for WindowsProcessJob {}
    unsafe impl Sync for WindowsProcessJob {}

    impl WindowsProcessJob {
        pub fn attach_and_resume(child: &Child) -> Result<Self, String> {
            let pid = child
                .id()
                .ok_or_else(|| "Windows 子进程在加入 Job Object 前已经退出。".to_string())?;
            let process_handle = child
                .raw_handle()
                .ok_or_else(|| "无法取得 Windows 子进程句柄。".to_string())?
                as HANDLE;
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "创建 Windows Job Object 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            let job = Self { handle };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = unsafe {
                SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(format!(
                    "配置 Windows Job Object 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            if unsafe { AssignProcessToJobObject(job.handle, process_handle) } == 0 {
                return Err(format!(
                    "将 Windows 子进程加入 Job Object 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            if let Err(error) = resume_initial_thread(pid) {
                let _ = job.terminate(1);
                return Err(error);
            }
            Ok(job)
        }

        pub fn terminate(&self, exit_code: u32) -> Result<(), String> {
            if unsafe { TerminateJobObject(self.handle, exit_code) } == 0 {
                return Err(format!(
                    "TerminateJobObject 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }

    impl Drop for WindowsProcessJob {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    fn resume_initial_thread(pid: u32) -> Result<(), String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "枚举 Windows 子进程初始线程失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        let snapshot = OwnedHandle(snapshot);
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut has_entry = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while has_entry {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    let thread = OwnedHandle(thread);
                    if unsafe { ResumeThread(thread.0) } == u32::MAX {
                        return Err(format!(
                            "恢复 Windows 子进程初始线程失败：{}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    return Ok(());
                }
            }
            has_entry = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }
        Err("没有找到 Windows 子进程初始线程，已拒绝执行。".to_string())
    }

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
