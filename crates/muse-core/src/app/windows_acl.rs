//! Windows 数据目录 DACL 收紧。
//!
//! Windows 没有等价于 chmod 0700 的权限模型，数据目录默认继承父目录 ACL，
//! 宽松 ACE 会一路传导到存放明文 API Key 的 `config.toml`。这里在每次启动时
//! 把数据目录改写为受保护 DACL：仅当前用户、SYSTEM、Administrators 拥有
//! FullControl，并通过 OICI 让目录内新建文件与子目录自动继承同样三条 ACE。

use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 把 `path` 指向的目录改写为受保护 DACL，仅授权当前用户、SYSTEM 与 Administrators。
///
/// 该函数幂等：重复调用只是重写同样的三条 ACE。任何一步失败都会返回错误，
/// 由调用方按 fail-closed 处理，与 Unix chmod 失败的行为一致。
pub(crate) fn restrict_directory_dacl(path: &Path) -> Result<(), String> {
    let user_sid = current_user_sid_string()?;
    let sddl = format!("D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    apply_sddl(path, &sddl)
}

/// 读取当前进程用户的 SID 字符串（形如 `S-1-5-21-...`）。
fn current_user_sid_string() -> Result<String, String> {
    let mut raw_token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess 返回的伪句柄始终有效；成功后 raw_token 归 OwnedToken 负责关闭。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) } == 0 {
        return Err(format!(
            "无法打开当前进程访问令牌：{}",
            std::io::Error::last_os_error()
        ));
    }
    let token = OwnedToken(raw_token);

    let mut length = 0u32;
    // SAFETY: 首次调用传空缓冲区只为取得所需长度，预期返回 ERROR_INSUFFICIENT_BUFFER。
    let probe_ok =
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut length) };
    if probe_ok != 0
        || std::io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || length == 0
    {
        return Err("无法测量当前用户安全标识符长度。".to_string());
    }
    // 以 u64 对齐分配，保证其后按 TOKEN_USER 读取指针对齐安全。
    let mut buffer = vec![0u64; (length as usize).div_ceil(size_of::<u64>())];
    // SAFETY: buffer 长度来自上一调用且本次调用内保持可写，ReturnLength 写回同一变量。
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(format!(
            "无法读取当前用户安全标识符：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: 上一调用成功保证 buffer 内含完整 TOKEN_USER，且 buffer 按 u64 对齐。
    let sid = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read().User.Sid };

    let mut sid_string = std::ptr::null_mut();
    // SAFETY: sid 指向 buffer 内有效 SID；成功后 sid_string 由 LocalAlloc 分配。
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_string) } == 0 {
        return Err(format!(
            "无法转换当前用户安全标识符：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: sid_string 是以 NUL 结尾的 UTF-16 串，复制到 String 后即可释放。
    let result = unsafe { wide_pointer_to_string(sid_string) };
    // SAFETY: sid_string 由 LocalAlloc 分配且尚未释放。
    unsafe { LocalFree(sid_string.cast()) };
    Ok(result)
}

/// 把 SDDL 描述的受保护 DACL 写入目录。
fn apply_sddl(path: &Path, sddl: &str) -> Result<(), String> {
    let sddl_wide = wide_nul_terminated(sddl);
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: sddl_wide 以 NUL 结尾；成功后 descriptor 由 LocalAlloc 分配，函数末尾释放。
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "无法生成数据目录安全描述符：{}",
            std::io::Error::last_os_error()
        ));
    }
    let descriptor = OwnedDescriptor(descriptor);

    let mut present = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: descriptor 在 OwnedDescriptor 生命周期内有效，dacl 指向其内部内存。
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        return Err("无法从安全描述符中提取数据目录访问控制列表。".to_string());
    }

    let path_wide = security_api_path(path)?;
    // SAFETY: path_wide 以 NUL 结尾；dacl 来自仍存活的 descriptor，SACL 与属主保持不变。
    let status = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!(
            "无法写入数据目录访问控制列表：{}",
            std::io::Error::from_raw_os_error(status as i32)
        ));
    }
    Ok(())
}

/// 把路径编码为安全 API 可接受的 NUL 结尾 UTF-16。
///
/// `SetNamedSecurityInfoW`/`GetNamedSecurityInfoW` 不接受 `\\?\` 扩展长度前缀，
/// 这里显式剥离（UNC 形式不属于数据目录场景，直接报错而不是静默改写）。
fn security_api_path(path: &Path) -> Result<Vec<u16>, String> {
    let text = path.as_os_str().to_string_lossy();
    let stripped = text
        .strip_prefix(r"\\?\")
        .filter(|rest| !rest.starts_with(r"UNC\"))
        .unwrap_or(&text);
    if stripped.starts_with(r"\\?\") {
        return Err("数据目录使用了安全 API 不支持的扩展长度路径。".to_string());
    }
    Ok(stripped.encode_utf16().chain(std::iter::once(0)).collect())
}

fn wide_nul_terminated(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 复制 NUL 结尾 UTF-16 指针的内容；调用方须保证指针有效且拥有释放责任。
///
/// # Safety
///
/// `ptr` 必须指向以 NUL 结尾的有效 UTF-16 缓冲区。
unsafe fn wide_pointer_to_string(ptr: *const u16) -> String {
    // SAFETY: 由调用方保证 ptr 指向 NUL 结尾缓冲区，循环在 NUL 处停止。
    let length = unsafe {
        let mut end = ptr;
        while *end != 0 {
            end = end.add(1);
        }
        end.offset_from(ptr) as usize
    };
    // SAFETY: length 是同一缓冲区内的元素数，切片不越界。
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, length) })
}

struct OwnedToken(HANDLE);

impl Drop for OwnedToken {
    fn drop(&mut self) {
        // SAFETY: 句柄由 OpenProcessToken 取得且仅在此关闭一次。
        unsafe { CloseHandle(self.0) };
    }
}

struct OwnedDescriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

impl Drop for OwnedDescriptor {
    fn drop(&mut self) {
        // SAFETY: 描述符由 LocalAlloc 分配（ConvertStringSecurityDescriptorToSecurityDescriptorW），
        // 按文档须以 LocalFree 释放且仅释放一次。
        unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, CONTAINER_INHERIT_ACE,
        GetAce, GetAclInformation, GetSecurityDescriptorControl, OBJECT_INHERIT_ACE,
        SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    const SYSTEM_SID: &str = "S-1-5-18";
    const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create(tag: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间应有效")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "muse-acl-test-{tag}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("应能创建测试目录");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 读回路径 DACL 的控制位与全部 Allow ACE 的 (SID, 访问掩码, ACE 标志)。
    fn read_allow_aces(path: &Path) -> (u16, Vec<(String, u32, u8)>) {
        let path_wide = security_api_path(path).expect("测试路径应可编码");
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: path_wide 以 NUL 结尾；成功后 descriptor 由系统分配，结束前 LocalFree。
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "读取安全描述符应成功");
        let descriptor = OwnedDescriptor(descriptor);

        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: descriptor 有效，输出指针指向局部变量。
        assert!(
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } != 0,
            "读取安全描述符控制位应成功"
        );

        let mut present = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: descriptor 有效，dacl 指向其内部内存。
        assert!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            } != 0
                && present != 0
                && !dacl.is_null(),
            "DACL 必须存在"
        );

        let mut size_info = ACL_SIZE_INFORMATION::default();
        // SAFETY: dacl 指向有效 ACL，size_info 缓冲区大小与结构体一致。
        assert!(
            unsafe {
                GetAclInformation(
                    dacl,
                    (&mut size_info as *mut ACL_SIZE_INFORMATION).cast(),
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } != 0,
            "读取 ACL 信息应成功"
        );

        let mut aces = Vec::new();
        for index in 0..size_info.AceCount {
            let mut ace_ptr = std::ptr::null_mut();
            // SAFETY: index 在 AceCount 范围内，GetAce 返回 ACL 内部 ACE 指针。
            assert!(unsafe { GetAce(dacl, index, &mut ace_ptr) } != 0);
            // SAFETY: ACE 指针来自有效 ACL，按 ACCESS_ALLOWED_ACE 布局读取。
            let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
            assert_eq!(
                ace.Header.AceType, ACCESS_ALLOWED_ACE_TYPE as u8,
                "收紧后的 DACL 不得包含 Allow 以外的 ACE"
            );
            // SAFETY: SidStart 之后就是内联 SID，其地址即 SID 起点；
            // 成功后 sid_string 由 LocalAlloc 分配。
            let mut sid_string = std::ptr::null_mut();
            assert!(
                unsafe {
                    ConvertSidToStringSidW(
                        (&raw const ace.SidStart).cast_mut().cast(),
                        &mut sid_string,
                    )
                } != 0
            );
            // SAFETY: sid_string 为 NUL 结尾 UTF-16。
            let sid_text = unsafe { wide_pointer_to_string(sid_string) };
            // SAFETY: sid_string 由 LocalAlloc 分配且仅释放一次。
            unsafe { LocalFree(sid_string.cast()) };
            aces.push((sid_text, ace.Mask, ace.Header.AceFlags));
        }
        (control, aces)
    }

    fn expected_trustees() -> Vec<String> {
        let user = current_user_sid_string().expect("应能读取当前用户 SID");
        vec![user, SYSTEM_SID.to_string(), ADMINISTRATORS_SID.to_string()]
    }

    #[test]
    fn restrict_directory_dacl_applies_protected_three_trustee_dacl() {
        let directory = TestDirectory::create("apply");
        restrict_directory_dacl(&directory.0).expect("收紧数据目录 DACL 应成功");

        let (control, aces) = read_allow_aces(&directory.0);
        assert_ne!(
            control & SE_DACL_PROTECTED,
            0,
            "DACL 必须标记为受保护，不再继承父目录 ACE"
        );
        let mut trustees = aces
            .iter()
            .map(|(sid, _, _)| sid.clone())
            .collect::<Vec<_>>();
        trustees.sort();
        let mut expected = expected_trustees();
        expected.sort();
        assert_eq!(trustees, expected, "Allow ACE 主体必须恰好是三个授权主体");
        for (_, mask, flags) in &aces {
            // SDDL 的 FA 落到文件对象上会被映射为 FILE_ALL_ACCESS。
            assert_eq!(*mask, FILE_ALL_ACCESS, "授权主体应拥有 FullControl");
            assert_eq!(
                flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8,
                (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8,
                "目录 ACE 必须带 OICI 使新建对象自动继承"
            );
        }
    }

    #[test]
    fn restrict_directory_dacl_is_idempotent() {
        let directory = TestDirectory::create("idempotent");
        restrict_directory_dacl(&directory.0).expect("首次收紧应成功");
        let first = read_allow_aces(&directory.0);
        restrict_directory_dacl(&directory.0).expect("重复收紧应成功");
        let second = read_allow_aces(&directory.0);
        assert_eq!(first, second, "重复收紧不得改变 DACL 内容");
    }

    #[test]
    fn new_file_inherits_restricted_dacl() {
        let directory = TestDirectory::create("inherit");
        restrict_directory_dacl(&directory.0).expect("收紧数据目录 DACL 应成功");

        let file_path = directory.0.join("config.toml");
        fs::write(&file_path, "key = \"value\"").expect("应能在目录内新建文件");

        let (_, aces) = read_allow_aces(&file_path);
        let mut trustees = aces
            .iter()
            .map(|(sid, _, _)| sid.clone())
            .collect::<Vec<_>>();
        trustees.sort();
        let mut expected = expected_trustees();
        expected.sort();
        assert_eq!(
            trustees, expected,
            "新建文件继承的 Allow ACE 主体必须恰好是三个授权主体"
        );
        for (_, mask, _) in &aces {
            assert_eq!(*mask, FILE_ALL_ACCESS, "继承的 ACE 应保持 FullControl");
        }
    }
}
