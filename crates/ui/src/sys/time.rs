//! Local wall-clock time for log timestamps.

use windows::Win32::System::SystemInformation::GetLocalTime;

/// `HH:MM:SS` in local time.
pub fn local_hms() -> String {
    // SAFETY: no arguments; returns a plain struct by value.
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}
