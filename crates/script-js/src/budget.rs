//! The run's time budget: a cooperative timeout checked on host calls.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

/// Decides whether a run may continue. Charged once per host call and once per replay attempt.
pub trait Budget {
    /// `false` stops the run with a time-limit error.
    fn charge(&mut self) -> bool;
}

/// No limit.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlimited;

impl Budget for Unlimited {
    fn charge(&mut self) -> bool {
        true
    }
}

/// A wall-clock limit for the whole run, replays included. The clock is read every `every`
/// charges to keep host calls cheap.
#[derive(Debug, Clone)]
pub struct WallClock {
    started: Option<Instant>,
    limit: Duration,
    every: u32,
    count: u32,
}

impl WallClock {
    pub fn new(limit: Duration, every: u32) -> Self {
        Self { started: None, limit, every: every.max(1), count: 0 }
    }
}

impl Budget for WallClock {
    fn charge(&mut self) -> bool {
        let started = *self.started.get_or_insert_with(Instant::now);
        self.count += 1;
        if self.count < self.every {
            return true;
        }
        self.count = 0;
        started.elapsed() <= self.limit
    }
}
