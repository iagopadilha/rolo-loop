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
        (ok != 0).then(|| ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
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
}
