//! Eased transitions for the data view. A tween's clock starts at the
//! first frame that advances it, so the input handlers that start one need
//! no clock.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    len: Duration,
    start: Option<Instant>,
    /// Linear progress, 0..=1.
    t: f32,
}

impl Tween {
    pub fn new(len: Duration) -> Self {
        Self {
            len,
            start: None,
            t: 0.0,
        }
    }

    /// Moves to `now`; returns the eased progress.
    pub fn advance(&mut self, now: Instant) -> f32 {
        let start = *self.start.get_or_insert(now);
        let len = self.len.as_secs_f32().max(1e-3);
        self.t = (now.saturating_duration_since(start).as_secs_f32() / len).min(1.0);
        self.eased()
    }

    /// Eased progress at the last advance, 0..=1.
    pub fn eased(&self) -> f32 {
        ease_out(self.t)
    }

    pub fn done(&self) -> bool {
        self.t >= 1.0
    }
}

/// Quick start, gentle landing.
pub fn ease_out(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// Moves `shown` toward `target` by at most `step`. A NaN `shown` (not yet
/// drawn) takes the target at once. Returns whether it is still short.
pub fn approach(shown: &mut f32, target: f32, step: f32) -> bool {
    let d = target - *shown;
    if d.is_nan() || d.abs() <= step {
        *shown = target;
        false
    } else {
        *shown += step.copysign(d);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tween_starts_on_first_advance_and_ends() {
        let mut tw = Tween::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert_eq!(tw.advance(t0 + Duration::from_secs(5)), 0.0);
        let mid = tw.advance(t0 + Duration::from_millis(5050));
        assert!(mid > 0.5 && mid < 1.0, "{mid}");
        assert!(!tw.done());
        assert_eq!(tw.advance(t0 + Duration::from_secs(6)), 1.0);
        assert!(tw.done());
    }

    #[test]
    fn approach_steps_and_lands() {
        let mut v = f32::NAN;
        assert!(!approach(&mut v, 1.0, 0.1));
        assert_eq!(v, 1.0);
        assert!(approach(&mut v, 0.0, 0.4));
        assert!((v - 0.6).abs() < 1e-6);
        assert!(approach(&mut v, 0.0, 0.4));
        assert!(!approach(&mut v, 0.0, 0.4));
        assert_eq!(v, 0.0);
    }
}
