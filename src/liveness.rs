//! Process liveness checks.
//!
//! Claude Code leaves `sessions\<pid>.json` behind when a session is killed rather than exited
//! cleanly, so every file has to be checked against the live process it names before it is
//! displayed.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

/// True when `pid` is the live process Claude Code recorded in the session file.
///
/// `proc_start` is that file's `procStart`: the process creation time in FILETIME ticks. When it is
/// there, matching it is the whole check. It pins the row to one process at 100ns resolution, which
/// is a stronger pid-reuse guard than the image name and, unlike the image name, survives an
/// in-place update of `claude.exe`.
///
/// Without it, fall back to the image name.
pub fn is_claude_process(pid: u32, proc_start: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        // A recorded creation time settles it on its own. If it can't be read, fall through to the
        // image name rather than dropping the session.
        if let (Some(expected), Some(actual)) = (proc_start, creation_ticks_of(handle)) {
            CloseHandle(handle);
            return expected == actual;
        }

        let verdict = image_is_claude(handle);
        CloseHandle(handle);
        verdict
    }
}

/// Process creation time in FILETIME ticks — the same value Claude Code writes as `procStart`.
///
/// Only the tests need to ask for a pid's creation time; `is_claude_process` reads it from a handle
/// it already holds.
#[cfg(test)]
fn creation_ticks(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let ticks = creation_ticks_of(handle);
        CloseHandle(handle);
        ticks
    }
}

unsafe fn creation_ticks_of(handle: HANDLE) -> Option<u64> {
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;

    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

/// Whether the process image is a Claude Code binary.
///
/// If the process exists but its path cannot be read (rare — a protected process holding a recycled
/// pid), we err toward showing the session: a stale row is less harmful here than a missing one.
unsafe fn image_is_claude(handle: HANDLE) -> bool {
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };

    if ok == 0 {
        return true;
    }

    let path = OsString::from_wide(&buf[..len as usize])
        .to_string_lossy()
        .to_ascii_lowercase();
    is_claude_leaf(path.rsplit(['\\', '/']).next().unwrap_or(""))
}

/// The image name of a Claude Code binary, lowercased.
///
/// `claude.exe.old.<timestamp>` counts: an in-place self-update renames the running binary aside,
/// and `QueryFullProcessImageNameW` reports the current on-disk name, so an exact `claude.exe`
/// match would hide every session older than the last update.
fn is_claude_leaf(file: &str) -> bool {
    file == "claude" || file.starts_with("claude.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A matching creation time is accepted on its own — this test binary is not `claude.exe`.
    #[test]
    fn a_matching_creation_time_identifies_the_process() {
        let pid = std::process::id();
        let ticks = creation_ticks(pid);
        assert!(ticks.is_some(), "no creation time for our own pid");
        assert!(is_claude_process(pid, ticks));
    }

    /// The point of the check: a recycled pid carries a different creation time.
    #[test]
    fn a_mismatched_creation_time_is_rejected() {
        assert!(!is_claude_process(std::process::id(), Some(1)));
    }

    #[test]
    fn the_renamed_binary_of_a_self_update_still_counts() {
        assert!(is_claude_leaf("claude.exe"));
        assert!(is_claude_leaf("claude"));
        assert!(is_claude_leaf("claude.exe.old.1788896357556"));
        assert!(!is_claude_leaf("claude-tray.exe"));
        assert!(!is_claude_leaf("notclaude.exe"));
    }
}
