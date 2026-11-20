// SPDX-License-Identifier: MIT
//! Process queries through the Windows API. This crate holds the workspace's only `unsafe`, each call wrapped in a safe function.

/// When the process `pid` started, as an integer that only means something compared with this function's value for the same pid
/// (the creation FILETIME). `None` when it cannot be known (dead process, no access, other systems).
#[cfg(windows)]
pub fn process_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return None;
    }
    // SAFETY: plain Win32 calls; the handle is closed on every path and the FILETIME out-pointers are valid for writes.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        (ok != 0).then_some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }
}

/// True when a process with this pid is running.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    // SAFETY: plain Win32 calls; the handle is closed before returning and the exit-code out-pointer is valid for writes.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

/// The file name of the program a process runs (`node.exe`), without its folder; `None` when it cannot be known (dead process, no access,
/// other systems). Used to tell the user WHICH program holds a folder, never to find a process to stop.
#[cfg(windows)]
pub fn process_image_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return None;
    }
    // SAFETY: plain Win32 calls; the handle is closed on every path and the buffer is valid for `size` UTF-16 units for the whole call.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string).filter(|n| !n.is_empty())
    }
}

#[cfg(not(windows))]
pub fn process_image_name(_pid: u32) -> Option<String> {
    None
}

/// A Windows job object: every process put in it, and every process those start afterwards, can be listed and ended together, even when the
/// parent that started them is already gone (a plain "kill the tree" cannot reach those orphans). The job ends everything it holds when the last
/// handle to it closes (the engine process ends, normally or not). A program that asks to break away from the job is allowed to (some tools
/// need it to start at all); it is then simply not tracked.
#[cfg(windows)]
pub struct Job {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: a job handle is a kernel object handle; the Win32 calls used here are safe to make from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}
// SAFETY: see above; the handle value is never mutated after creation.
#[cfg(windows)]
unsafe impl Sync for Job {}

#[cfg(windows)]
impl Job {
    /// A new job that ends its processes when it is closed; `None` when the system refuses.
    pub fn new() -> Option<Job> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: a null security descriptor and a null name are allowed; the info struct is zeroed (valid for this plain-data type) and is
        // valid for reads for the size passed; the handle is closed on the failure path.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return None;
            }
            Some(Job { handle })
        }
    }

    /// Puts a running process (and what it starts from now on) in the job. False when it cannot (already gone, no access).
    pub fn assign(&self, pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
        if pid == 0 {
            return false;
        }
        // SAFETY: plain Win32 calls; the process handle is closed on every path.
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return false;
            }
            let ok = AssignProcessToJobObject(self.handle, process);
            CloseHandle(process);
            ok != 0
        }
    }

    /// The pids of the processes in the job right now (at most 512).
    pub fn pids(&self) -> Vec<u32> {
        use windows_sys::Win32::System::JobObjects::{JobObjectBasicProcessIdList, QueryInformationJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST};
        const ROOM: usize = 512;
        // The struct is two u32 counters followed by a pointer-sized array: a buffer of usize words holds it with the right alignment.
        let mut buffer = vec![0usize; 1 + ROOM];
        // SAFETY: the buffer is valid for writes of `size` bytes for the whole call; the counters read back are bounded by ROOM.
        unsafe {
            let size = (buffer.len() * std::mem::size_of::<usize>()) as u32;
            let ok = QueryInformationJobObject(self.handle, JobObjectBasicProcessIdList, buffer.as_mut_ptr().cast(), size, std::ptr::null_mut());
            if ok == 0 {
                return vec![];
            }
            let list = &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST);
            let count = (list.NumberOfProcessIdsInList as usize).min(ROOM);
            let first = list.ProcessIdList.as_ptr();
            (0..count).map(|i| *first.add(i) as u32).collect()
        }
    }

    /// Ends every process in the job at once.
    pub fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: plain Win32 call on a handle this struct owns.
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this struct and closed once; closing the last handle ends the job's processes (kill on close).
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// Other systems have no job objects: nothing is tracked (the engine falls back to the registry file and `taskkill`-style tree kills).
#[cfg(not(windows))]
pub struct Job;

#[cfg(not(windows))]
impl Job {
    pub fn new() -> Option<Job> {
        None
    }
    pub fn assign(&self, _pid: u32) -> bool {
        false
    }
    pub fn pids(&self) -> Vec<u32> {
        vec![]
    }
    pub fn terminate(&self) {}
}

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
unsafe extern "system" fn on_console_event(kind: u32) -> i32 {
    // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1
    if kind <= 1 {
        INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
        return 1;
    }
    0
}

/// Makes Ctrl+C set the flag read by [`interrupted`] instead of ending the process at once (the loop then stops cleanly).
#[cfg(windows)]
pub fn install_ctrl_c_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: registers a handler that only stores to an atomic; the function pointer lives for the whole process.
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_event), 1);
    }
}

#[cfg(not(windows))]
pub fn install_ctrl_c_handler() {}

/// `count` bytes from the operating system's random generator (`BCryptGenRandom` with the system-preferred RNG).
#[cfg(windows)]
pub fn random_bytes(count: usize) -> Vec<u8> {
    use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut buffer = vec![0u8; count];
    if count == 0 {
        return buffer;
    }
    // SAFETY: the buffer is valid for `count` bytes for the whole call; a null algorithm handle is allowed with the system RNG flag.
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), buffer.as_mut_ptr(), count as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(status >= 0, "the system random generator failed (status {status:#x})");
    buffer
}

#[cfg(not(windows))]
pub fn random_bytes(count: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; count];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut file, &mut buffer);
    }
    buffer
}

/// True once Ctrl+C was pressed after [`install_ctrl_c_handler`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Clears the flag (tests).
pub fn clear_interrupt() {
    INTERRUPTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// The PATH text with `dir` added at the end (`add`) or every copy of it removed; `None` when nothing changes. Entries compare without case
/// and without a trailing backslash; empty entries are dropped only when something changes.
pub fn path_with_entry(path: &str, dir: &str, add: bool) -> Option<String> {
    let key = |entry: &str| entry.trim().trim_end_matches(['\\', '/']).to_lowercase();
    let wanted = key(dir);
    if wanted.is_empty() {
        return None;
    }
    let entries: Vec<&str> = path.split(';').filter(|e| !e.trim().is_empty()).collect();
    let present = entries.iter().any(|e| key(e) == wanted);
    match (add, present) {
        (true, true) | (false, false) => None,
        (true, false) => Some(entries.iter().copied().chain(std::iter::once(dir.trim())).collect::<Vec<_>>().join(";")),
        (false, true) => Some(entries.into_iter().filter(|e| key(e) != wanted).collect::<Vec<_>>().join(";")),
    }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The per-user PATH as stored in `HKCU\Environment`: the text (unexpanded) and whether it is `REG_EXPAND_SZ`. A missing value is
/// `("", true)`.
#[cfg(windows)]
pub fn read_user_path() -> std::io::Result<(String, bool)> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ};
    let (subkey, name) = (wide("Environment"), wide("Path"));
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: the key handle is closed below; the buffers outlive the calls and their sizes are passed in bytes.
    unsafe {
        let opened = RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key);
        if opened != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(opened as i32));
        }
        let mut size: u32 = 0;
        let mut kind: u32 = 0;
        let probe = RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size);
        if probe == ERROR_FILE_NOT_FOUND {
            RegCloseKey(key);
            return Ok((String::new(), true));
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut bytes = (buffer.len() * 2) as u32;
        let read = RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, buffer.as_mut_ptr().cast(), &mut bytes);
        RegCloseKey(key);
        if read != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(read as i32));
        }
        let length = buffer.iter().take((bytes as usize) / 2).position(|c| *c == 0).unwrap_or((bytes as usize) / 2);
        Ok((String::from_utf16_lossy(&buffer[..length]), kind == windows_sys::Win32::System::Registry::REG_EXPAND_SZ))
    }
}

/// Writes the per-user PATH with the type it had (`expandable` = `REG_EXPAND_SZ`, else `REG_SZ`) and tells running programs (Explorer,
/// new terminals) that the environment changed.
#[cfg(windows)]
pub fn write_user_path(value: &str, expandable: bool) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE};
    let (subkey, name, data, area) = (wide("Environment"), wide("Path"), wide(value), wide("Environment"));
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: the key handle is closed below; `data` is a NUL-terminated UTF-16 buffer whose byte size is passed; the broadcast only reads `area`.
    unsafe {
        let opened = RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut key);
        if opened != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(opened as i32));
        }
        let written = RegSetValueExW(key, name.as_ptr(), 0, if expandable { REG_EXPAND_SZ } else { REG_SZ }, data.as_ptr().cast(), (data.len() * 2) as u32);
        RegCloseKey(key);
        if written != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(written as i32));
        }
        let mut result: usize = 0;
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, area.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut result);
    }
    Ok(())
}

/// `CREATE_NO_WINDOW`: a console child gets no console window of its own.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The creation flag that keeps a console child from opening a window: [`CREATE_NO_WINDOW`] when this process has no console window
/// (the desktop app, a detached loop, a hook run by one of them), 0 when it runs in a terminal (the child then shares that terminal, so a
/// command-line user still sees its output).
#[cfg(windows)]
pub fn hidden_console_flags() -> u32 {
    // SAFETY: no arguments; it only returns the console window handle of this process, or null.
    let window = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
    if window.is_null() {
        CREATE_NO_WINDOW
    } else {
        0
    }
}

#[cfg(not(windows))]
pub fn hidden_console_flags() -> u32 {
    0
}

/// Sets `extra_flags | `[`hidden_console_flags`] as the Windows creation flags of `command` (nothing elsewhere). Every program this
/// workspace starts goes through here, so no terminal window ever flashes on the user's screen.
pub fn hide_console(command: &mut std::process::Command, extra_flags: u32) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(extra_flags | hidden_console_flags());
    }
    #[cfg(not(windows))]
    let _ = extra_flags;
    command
}

#[cfg(not(windows))]
pub fn read_user_path() -> std::io::Result<(String, bool)> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the user PATH lives in the Windows registry"))
}

#[cfg(not(windows))]
pub fn write_user_path(_value: &str, _expandable: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the user PATH lives in the Windows registry"))
}

#[cfg(not(windows))]
pub fn process_start_time(_pid: u32) -> Option<u64> {
    None
}

#[cfg(not(windows))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive_with_a_stable_start() {
        let pid = std::process::id();
        assert!(pid_alive(pid));
        assert_eq!(process_start_time(pid), process_start_time(pid));
        assert!(cfg!(not(windows)) || process_start_time(pid).is_some());
        assert!(!pid_alive(0) && process_start_time(0).is_none());
    }

    #[test]
    fn image_name_of_the_own_process_and_of_nothing() {
        assert!(cfg!(not(windows)) || process_image_name(std::process::id()).is_some_and(|n| n.to_lowercase().ends_with(".exe")));
        assert!(process_image_name(0).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_job_lists_and_ends_a_process_and_what_it_started() {
        use std::os::windows::process::CommandExt;
        let job = Job::new().expect("job");
        let mut child = std::process::Command::new("cmd").args(["/C", "ping -n 40 127.0.0.1 > nul"]).creation_flags(0x0800_0000).spawn().unwrap();
        let root = child.id();
        assert!(job.assign(root));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let members = job.pids();
        assert!(members.contains(&root), "{members:?}");
        assert!(members.len() >= 2, "the program the shell started is in the job too: {members:?}");
        job.terminate();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(members.iter().all(|pid| !pid_alive(*pid)), "{members:?}");
        let _ = child.wait();
        assert!(!job.assign(0));
    }

    #[cfg(windows)]
    #[test]
    fn closing_a_job_ends_its_processes() {
        use std::os::windows::process::CommandExt;
        let job = Job::new().expect("job");
        let mut child = std::process::Command::new("cmd").args(["/C", "ping -n 40 127.0.0.1 > nul"]).creation_flags(0x0800_0000).spawn().unwrap();
        let root = child.id();
        assert!(job.assign(root) && pid_alive(root));
        drop(job);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!pid_alive(root));
        let _ = child.wait();
    }

    #[test]
    fn path_entries_are_added_once_and_removed_everywhere() {
        let path = r"C:\Windows;%USERPROFILE%\bin;;C:\Tools\";
        assert_eq!(path_with_entry(path, r"C:\Apps\Maestro\plugin\bin", true).unwrap(), r"C:\Windows;%USERPROFILE%\bin;C:\Tools\;C:\Apps\Maestro\plugin\bin");
        assert_eq!(path_with_entry(path, r"c:\tools", true), None);
        assert_eq!(path_with_entry(path, r"C:\Tools", false).unwrap(), r"C:\Windows;%USERPROFILE%\bin");
        assert_eq!(path_with_entry(r"C:\A;c:\a\;C:\B", r"C:\A", false).unwrap(), r"C:\B");
        assert_eq!(path_with_entry(path, r"C:\Nope", false), None);
        assert_eq!(path_with_entry("", r"C:\A", true).unwrap(), r"C:\A");
        assert_eq!(path_with_entry(path, "  ", true), None);
    }
}
