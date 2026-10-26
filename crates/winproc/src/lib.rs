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
