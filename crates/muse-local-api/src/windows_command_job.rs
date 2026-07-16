//! Windows 命令进程树隔离：子进程先挂起，加入 Job Object 后才允许执行。

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

pub(crate) const CREATE_SUSPENDED_NO_WINDOW_FLAGS: u32 = CREATE_SUSPENDED | CREATE_NO_WINDOW;

/// 独占一个命令进程树；关闭句柄时由系统终止仍留在树中的全部进程。
pub(crate) struct WindowsCommandJob {
    handle: HANDLE,
}

// SAFETY: handle 是本类型独占且只通过线程安全的内核 Job Object API 使用，Drop 只关闭一次。
unsafe impl Send for WindowsCommandJob {}
// SAFETY: 并发借用只会调用 TerminateJobObject；句柄关闭由独占的 Drop 执行。
unsafe impl Sync for WindowsCommandJob {}

impl WindowsCommandJob {
    /// 创建并配置 Job Object，将仍处于挂起态的进程加入后，再恢复其初始线程。
    pub(crate) fn attach_and_resume(child: &Child) -> Result<Self, String> {
        let pid = child
            .id()
            .ok_or_else(|| "Windows 命令进程在加入 Job Object 前已经退出。".to_string())?;
        let process_handle = child
            .raw_handle()
            .ok_or_else(|| "无法取得 Windows 命令进程句柄。".to_string())?
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
                "配置 Windows Job Object 的 KILL_ON_JOB_CLOSE 失败：{}",
                std::io::Error::last_os_error()
            ));
        }

        let assigned = unsafe { AssignProcessToJobObject(job.handle, process_handle) };
        if assigned == 0 {
            return Err(format!(
                "将 Windows 命令进程加入 Job Object 失败：{}",
                std::io::Error::last_os_error()
            ));
        }

        if let Err(error) = resume_initial_thread(pid) {
            let _ = job.terminate(1);
            return Err(error);
        }
        Ok(job)
    }

    /// 终止 Job Object 中的根进程及全部子孙进程。
    pub(crate) fn terminate(&self, exit_code: u32) -> Result<(), String> {
        let terminated = unsafe { TerminateJobObject(self.handle, exit_code) };
        if terminated == 0 {
            return Err(format!(
                "TerminateJobObject 失败：{}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl Drop for WindowsCommandJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE 保证即使取消路径异常退出，仍不会留下孤儿子进程。
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn resume_initial_thread(pid: u32) -> Result<(), String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "枚举 Windows 命令初始线程失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
    let mut open_error = None;

    while has_entry {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                open_error = Some(std::io::Error::last_os_error());
            } else {
                let thread = OwnedHandle(thread);
                let previous_suspend_count = unsafe { ResumeThread(thread.0) };
                if previous_suspend_count == u32::MAX {
                    return Err(format!(
                        "恢复 Windows 命令初始线程失败：{}",
                        std::io::Error::last_os_error()
                    ));
                }
                return Ok(());
            }
        }
        has_entry = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
    }

    match open_error {
        Some(error) => Err(format!("打开 Windows 命令初始线程失败：{error}")),
        None => Err("没有找到 Windows 命令进程的初始线程，已拒绝执行。".to_string()),
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
