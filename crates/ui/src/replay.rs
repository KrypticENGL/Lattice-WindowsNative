//! Replay of a recorded run: the snapshot at every line the program
//! stepped through, played back like a media player.
//!
//! Play records the whole program first (see `debug.rs`), so the number of
//! steps is known before playback starts. The last step is the last stop
//! before the program exited; playback ends there and keeps it on screen.

use std::time::{Duration, Instant};

use debugger::Snapshot;

use crate::sys::Waker;

/// How long each step shows at 1× speed.
pub const STEP_TIME: Duration = Duration::from_millis(600);

/// Playback speeds in percent, cycled by the speed button.
const SPEEDS: [u32; 4] = [50, 100, 200, 400];
const NORMAL_SPEED: usize = 1;

/// What the play bar shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayStatus {
    /// 0-based.
    pub pos: usize,
    pub len: usize,
    pub playing: bool,
    pub speed: f32,
}

/// Play bar commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayAction {
    /// Play or pause (or record when there is nothing to replay).
    Toggle,
    /// Jump to a step (0-based), keeping play or pause.
    Seek(usize),
    /// Pause and move by this many steps.
    Step(isize),
    /// Next playback speed.
    Speed,
    /// Record the program again.
    Record,
}

pub struct Replay {
    steps: Vec<Snapshot>,
    pos: usize,
    playing: bool,
    speed: usize,
    /// When the next step is due while playing.
    next_at: Option<Instant>,
}

impl Replay {
    /// A replay at its first step, playing. `None` if nothing was recorded.
    pub fn new(steps: Vec<Snapshot>, now: Instant) -> Option<Self> {
        if steps.is_empty() {
            return None;
        }
        let mut r = Self {
            steps,
            pos: 0,
            playing: false,
            speed: NORMAL_SPEED,
            next_at: None,
        };
        r.play(now);
        Some(r)
    }

    pub fn status(&self) -> ReplayStatus {
        ReplayStatus {
            pos: self.pos,
            len: self.steps.len(),
            playing: self.playing,
            speed: SPEEDS[self.speed] as f32 / 100.0,
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.steps[self.pos]
    }

    fn last(&self) -> usize {
        self.steps.len() - 1
    }

    fn interval(&self) -> Duration {
        STEP_TIME * 100 / SPEEDS[self.speed]
    }

    fn play(&mut self, now: Instant) {
        if self.pos == self.last() {
            if self.steps.len() == 1 {
                return;
            }
            // At the end, play starts over like a media player.
            self.pos = 0;
        }
        self.playing = true;
        self.next_at = Some(now + self.interval());
    }

    fn pause(&mut self) {
        self.playing = false;
        self.next_at = None;
    }

    /// Applies a play bar command. Returns `true` if the step changed.
    pub fn apply(&mut self, a: ReplayAction, now: Instant) -> bool {
        let before = self.pos;
        match a {
            ReplayAction::Toggle if self.playing => self.pause(),
            ReplayAction::Toggle => self.play(now),
            ReplayAction::Seek(i) => {
                self.pos = i.min(self.last());
                if self.playing {
                    if self.pos == self.last() {
                        self.pause();
                    } else {
                        self.next_at = Some(now + self.interval());
                    }
                }
            }
            ReplayAction::Step(d) => {
                self.pause();
                self.pos = self.pos.saturating_add_signed(d).min(self.last());
            }
            ReplayAction::Speed => {
                self.speed = (self.speed + 1) % SPEEDS.len();
                if self.playing {
                    self.next_at = Some(now + self.interval());
                }
            }
            ReplayAction::Record => {}
        }
        self.pos != before
    }

    /// Advances if the next step is due. Returns `true` if it moved. Play
    /// stops on the last step.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(at) = self.next_at.filter(|_| self.playing) else {
            return false;
        };
        if now < at {
            return false;
        }
        self.pos += 1;
        if self.pos >= self.last() {
            self.pos = self.last();
            self.pause();
        } else {
            // From the due time, not `now`, so a late wake does not drift.
            self.next_at = Some((at + self.interval()).max(now));
        }
        true
    }

    /// Time until the next step, while playing.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.next_at.map(|t| t.saturating_duration_since(now))
    }
}

/// Wakes the UI thread after `delay` (the replay's step timer).
pub fn wake_after(waker: &Waker, delay: Duration) {
    let waker = waker.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        waker.wake();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(n: usize) -> Vec<Snapshot> {
        (0..n).map(|_| Snapshot::default()).collect()
    }

    #[test]
    fn plays_to_the_last_step_and_stops_there() {
        let t0 = Instant::now();
        let mut r = Replay::new(steps(3), t0).unwrap();
        assert_eq!(r.status().pos, 0);
        assert!(r.status().playing);
        assert!(!r.tick(t0), "not due yet");
        let mut t = t0;
        for want in [1, 2] {
            t += STEP_TIME;
            assert!(r.tick(t));
            assert_eq!(r.status().pos, want);
        }
        assert!(!r.status().playing, "stops on the last step");
        assert!(!r.tick(t + STEP_TIME * 10));
        assert_eq!(r.status().pos, 2);
        assert_eq!(r.next_due(t), None);
    }

    #[test]
    fn play_at_the_end_starts_over() {
        let t0 = Instant::now();
        let mut r = Replay::new(steps(4), t0).unwrap();
        r.apply(ReplayAction::Seek(99), t0);
        assert_eq!(r.status().pos, 3);
        assert!(!r.status().playing);
        assert!(r.apply(ReplayAction::Toggle, t0));
        assert_eq!(r.status().pos, 0);
        assert!(r.status().playing);
    }

    #[test]
    fn stepping_pauses_and_clamps() {
        let t0 = Instant::now();
        let mut r = Replay::new(steps(5), t0).unwrap();
        assert!(!r.apply(ReplayAction::Step(-1), t0));
        assert!(!r.status().playing);
        assert!(r.apply(ReplayAction::Step(3), t0));
        assert_eq!(r.status().pos, 3);
        r.apply(ReplayAction::Step(10), t0);
        assert_eq!(r.status().pos, 4);
    }

    #[test]
    fn speed_shortens_the_step() {
        let t0 = Instant::now();
        let mut r = Replay::new(steps(5), t0).unwrap();
        r.apply(ReplayAction::Speed, t0);
        assert_eq!(r.status().speed, 2.0);
        assert!(r.tick(t0 + STEP_TIME / 2));
        assert_eq!(r.status().pos, 1);
    }

    #[test]
    fn nothing_recorded_is_no_replay() {
        assert!(Replay::new(Vec::new(), Instant::now()).is_none());
        let one = Replay::new(steps(1), Instant::now()).unwrap();
        assert!(!one.status().playing);
    }
}
