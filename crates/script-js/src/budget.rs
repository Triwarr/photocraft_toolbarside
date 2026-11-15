//! The run's time budget: a cooperative timeout checked on host calls.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

/// Decides whether a run may continue. Charged once per host call and once per replay attempt.
pub trait Budget {
    /// `false` stops the run with a time-limit error.
    fn charge(&mut self) -> bool;
    /// The run stopped executing, e.g. at a dialog. Time until `resume` is not charged.
    fn pause(&mut self) {}
    /// The run executes again.
    fn resume(&mut self) {}
}

/// No limit.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlimited;

impl Budget for Unlimited {
    fn charge(&mut self) -> bool {
        true
    }
}

/// A wall-clock limit on the time the run spends executing, replays included. Time while the
/// run is paused at a dialog does not count. The clock is read every `every` charges to keep
/// host calls cheap.
#[derive(Debug, Clone)]
pub struct WallClock {
    spent: Duration,
    running_since: Option<Instant>,
    limit: Duration,
    every: u32,
    count: u32,
}

impl WallClock {
    pub fn new(limit: Duration, every: u32) -> Self {
        Self { spent: Duration::ZERO, running_since: None, limit, every: every.max(1), count: 0 }
    }

    fn elapsed(&self) -> Duration {
        self.spent + self.running_since.map_or(Duration::ZERO, |t| t.elapsed())
    }
}

impl Budget for WallClock {
    fn charge(&mut self) -> bool {
        self.resume();
        self.count += 1;
        if self.count < self.every {
            return true;
        }
        self.count = 0;
        self.elapsed() <= self.limit
    }

    fn pause(&mut self) {
        if let Some(t) = self.running_since.take() {
            self.spent += t.elapsed();
        }
    }

    fn resume(&mut self) {
        if self.running_since.is_none() {
            self.running_since = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_time_is_not_charged() {
        let mut b = WallClock::new(Duration::from_millis(50), 1);
        assert!(b.charge());
        b.pause();
        std::thread::sleep(Duration::from_millis(100));
        assert!(b.charge());
        std::thread::sleep(Duration::from_millis(100));
        assert!(!b.charge());
    }
}
