//! Per-monitor v2 DPI awareness and DIP-to-pixel scaling.

use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use super::Error;

/// The DPI at which one device-independent pixel equals one physical pixel.
pub const BASE_DPI: u32 = 96;

/// Opts the process into per-monitor v2 DPI awareness. Call once, before
/// creating any window.
pub fn enable_per_monitor_v2() -> Result<(), Error> {
    // SAFETY: takes a predefined awareness-context constant and touches only
    // process-wide state; no pointers are involved.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .map_err(|e| Error::win32("SetProcessDpiAwarenessContext", e))
}

/// Converts device-independent pixels to physical pixels at `dpi`, rounding
/// to the nearest pixel.
pub fn scale(dips: i32, dpi: u32) -> i32 {
    let dpi = i64::from(dpi);
    let base = i64::from(BASE_DPI);
    let scaled = (i64::from(dips) * dpi + base / 2).div_euclid(base);
    i32::try_from(scaled).unwrap_or(if dips < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_is_identity_at_base_dpi() {
        assert_eq!(scale(1280, BASE_DPI), 1280);
    }

    #[test]
    fn scale_rounds_common_factors() {
        assert_eq!(scale(1280, 144), 1920); // 150%
        assert_eq!(scale(800, 120), 1000); // 125%
        assert_eq!(scale(1, 120), 1); // 1.25 rounds down
        assert_eq!(scale(3, 120), 4); // 3.75 rounds up
    }

    #[test]
    fn scale_saturates_instead_of_overflowing() {
        assert_eq!(scale(i32::MAX, 192), i32::MAX);
    }
}
