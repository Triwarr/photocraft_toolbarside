//! One script run: attempts, pauses at modal dialogs, and the final result.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::context::time::{Clock, JsInstant, StdClock};
use boa_engine::{Context, Source};
use serde_json::Value;

use crate::budget::Budget;
use crate::host::Host;
use crate::tape::{Call, Tape};
use crate::{APP_NAME, abort, natives, preprocess};

/// Loop iterations allowed per call frame before the run fails (Boa counts per frame on 0.22).
const LOOP_LIMIT: u64 = 100_000_000;
const RECURSION_LIMIT: usize = 512;

/// Per-run settings.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// `app.name` for this run only (the first tag of a loaded script). `None` keeps the default.
    pub app_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Alert,
    Confirm,
    Prompt,
}

/// A dialog the UI must answer before the run can continue.
#[derive(Debug, Clone, PartialEq)]
pub struct Modal {
    pub kind: ModalKind,
    /// The script's arguments, as JSON (`[message, ...]`).
    pub args: Value,
}

/// Where a run stands after [`Runner::step`].
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The script finished.
    Done,
    /// Waiting for [`Runner::answer`].
    Paused(Modal),
    /// The script failed or was stopped; the message is for the user.
    Failed(String),
}

/// Why an attempt was aborted.
pub(crate) enum Stop {
    Paused,
    TimeLimit,
    Fault(String),
}

pub(crate) struct State {
    pub tape: Tape,
    pub host: Box<dyn Host>,
    pub budget: Box<dyn Budget>,
    pub stop: Option<Stop>,
    pub app_name: String,
    pub attempts: u32,
    rng: u64,
}

pub(crate) type Shared = Rc<RefCell<State>>;

/// `Date` reads the wall clock from the tape, so a replay sees the times the first attempt saw.
struct TapeClock {
    shared: Shared,
    monotonic: StdClock,
}

impl Clock for TapeClock {
    fn now(&self) -> JsInstant {
        self.monotonic.now()
    }

    fn system_time_millis(&self) -> i64 {
        let live = || self.monotonic.system_time_millis();
        match self.shared.try_borrow_mut() {
            Ok(mut s) => s.tape.clock(live),
            Err(_) => live(),
        }
    }
}

impl State {
    /// A fresh `Math.random()` value (xorshift seeded from the clock; taped by the caller).
    pub fn next_random(&mut self) -> f64 {
        if self.rng == 0 {
            let seed = StdClock::new().system_time_millis().unsigned_abs();
            self.rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A script run. Not `Send`: on native targets it lives on the script thread.
pub struct Runner {
    source: String,
    shared: Shared,
    finished: Option<Step>,
}

impl Runner {
    /// # Errors
    /// A preprocessing error (an unsupported directive).
    pub fn new(source: &str, host: Box<dyn Host>, budget: Box<dyn Budget>, options: RunOptions) -> Result<Self, String> {
        let source = preprocess::preprocess(source)?;
        let state =
            State { tape: Tape::default(), host, budget, stop: None, app_name: options.app_name.unwrap_or_else(|| APP_NAME.to_owned()), attempts: 0, rng: 0 };
        Ok(Self { source, shared: Rc::new(RefCell::new(state)), finished: None })
    }

    /// Attempts made so far (the first run plus one replay per answered dialog).
    pub fn attempts(&self) -> u32 {
        self.shared.borrow().attempts
    }

    /// Run until the script finishes, fails, or opens a dialog that has no answer yet.
    pub fn step(&mut self) -> Step {
        if let Some(done) = &self.finished {
            return done.clone();
        }
        if let Some(modal) = self.pending() {
            return Step::Paused(modal);
        }
        let step = self.attempt();
        if !matches!(step, Step::Paused(_)) {
            self.finished = Some(step.clone());
        }
        step
    }

    /// Answer the dialog the run is paused at. `alert` takes `null`, `confirm` a boolean and
    /// `prompt` a string, or `null` for Cancel.
    ///
    /// # Errors
    /// When the run is not paused.
    pub fn answer(&mut self, value: Value) -> Result<(), String> {
        self.shared.borrow_mut().tape.answer(value)
    }

    fn pending(&self) -> Option<Modal> {
        let s = self.shared.borrow();
        let (_, e) = s.tape.pending_modal()?;
        match &e.call {
            Call::Modal { kind, args } => Some(Modal { kind: *kind, args: args.clone() }),
            _ => None,
        }
    }

    fn attempt(&mut self) -> Step {
        {
            let mut s = self.shared.borrow_mut();
            s.tape.rewind();
            s.stop = None;
            s.attempts += 1;
            if !s.budget.charge() {
                return Step::Failed(time_limit_message());
            }
        }
        let result = self.fresh_context().and_then(|mut ctx| ctx.eval(Source::from_bytes(&self.source)).map(drop));
        let mut s = self.shared.borrow_mut();
        match s.stop.take() {
            Some(Stop::Paused) => {
                drop(s);
                self.pending().map_or_else(|| Step::Failed("internal error: paused without a dialog".into()), Step::Paused)
            }
            Some(Stop::TimeLimit) => Step::Failed(time_limit_message()),
            Some(Stop::Fault(m)) => Step::Failed(m),
            None => match result {
                Err(e) => Step::Failed(e.to_string()),
                Ok(()) if s.tape.unconsumed() > 0 => {
                    Step::Failed(format!("replay divergence: the script ended with {} recorded calls left", s.tape.unconsumed()))
                }
                Ok(()) => Step::Done,
            },
        }
    }

    fn fresh_context(&self) -> boa_engine::JsResult<Context> {
        let clock = Rc::new(TapeClock { shared: self.shared.clone(), monotonic: StdClock::new() });
        let mut ctx = Context::builder().clock(clock).build()?;
        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_LIMIT);
        ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);
        let empty = abort::empty_function(&mut ctx)?;
        ctx.insert_data(natives::Abort(empty));
        ctx.insert_data(self.shared.clone());
        natives::install(&mut ctx)?;
        Ok(ctx)
    }
}

fn time_limit_message() -> String {
    "The script was stopped: it ran longer than its time limit.".into()
}
