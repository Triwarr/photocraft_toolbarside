//! The record of everything a run did outside the engine, in call order.
//!
//! The first attempt appends entries; every later attempt of the same run replays them. A
//! replayed call must ask for exactly what was recorded at that position (same kind, same
//! arguments); anything else is a divergence and fails the run instead of re-applying effects.

use serde_json::Value;

/// What a recorded call asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    /// A session command by id, with its JSON params.
    Command { id: String, params: Value },
    /// A line written to the script output.
    Output(String),
    /// A modal dialog (`alert`, `confirm`, `prompt`) with its arguments.
    Modal { kind: crate::ModalKind, args: Value },
}

/// How a recorded call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Returned this value.
    Return(Value),
    /// Failed with this message (seen by the script as a catchable `Error`).
    Throw(String),
    /// A modal dialog: `None` while the UI has not answered it yet.
    Modal(Option<Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub call: Call,
    pub outcome: Outcome,
}

/// Where the next call stands relative to the tape.
pub enum Begin {
    /// Already recorded: answer from the tape.
    Replay(Outcome),
    /// Past the end of the tape: perform it live, then [`Tape::finish`] it at this index.
    Live(usize),
}

#[derive(Debug, Default)]
pub struct Tape {
    entries: Vec<Entry>,
    cursor: usize,
    clock: Vec<i64>,
    clock_cursor: usize,
    random: Vec<f64>,
    random_cursor: usize,
}

impl Tape {
    /// Rewind for a new attempt; recorded entries stay.
    pub fn rewind(&mut self) {
        self.cursor = 0;
        self.clock_cursor = 0;
        self.random_cursor = 0;
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Recorded entries the current attempt has not reached.
    pub fn unconsumed(&self) -> usize {
        self.entries.len() - self.cursor
    }

    /// Start the next call.
    ///
    /// # Errors
    /// A divergence message when the tape holds a different call at this position.
    pub fn begin(&mut self, call: Call) -> Result<Begin, String> {
        let at = self.cursor;
        self.cursor += 1;
        match self.entries.get(at) {
            Some(e) if e.call == call => Ok(Begin::Replay(e.outcome.clone())),
            Some(e) => Err(format!("replay divergence at call {at}: recorded {:?}, script asked for {:?}", e.call, call)),
            None => {
                // Placeholder outcome until the live call finishes.
                self.entries.push(Entry { call, outcome: Outcome::Modal(None) });
                Ok(Begin::Live(at))
            }
        }
    }

    /// Record how a live call ended.
    pub fn finish(&mut self, at: usize, outcome: Outcome) {
        if let Some(e) = self.entries.get_mut(at) {
            e.outcome = outcome;
        }
    }

    /// The open modal waiting for an answer, if any (always the last entry).
    pub fn pending_modal(&self) -> Option<(usize, &Entry)> {
        let at = self.entries.len().checked_sub(1)?;
        let e = &self.entries[at];
        matches!(e.outcome, Outcome::Modal(None)).then_some((at, e))
    }

    /// Answer the pending modal.
    ///
    /// # Errors
    /// When no modal is waiting.
    pub fn answer(&mut self, value: Value) -> Result<(), String> {
        let at = self.pending_modal().map(|(i, _)| i).ok_or("no dialog is waiting for an answer")?;
        self.entries[at].outcome = Outcome::Modal(Some(value));
        Ok(())
    }

    /// The wall-clock reading for `Date`: recorded, or `live()` recorded now.
    pub fn clock(&mut self, live: impl FnOnce() -> i64) -> i64 {
        let v = match self.clock.get(self.clock_cursor) {
            Some(&v) => v,
            None => {
                let v = live();
                self.clock.push(v);
                v
            }
        };
        self.clock_cursor += 1;
        v
    }

    /// The next recorded `Math.random()` value, if this attempt has not used them all.
    pub fn take_random(&mut self) -> Option<f64> {
        let v = self.random.get(self.random_cursor).copied()?;
        self.random_cursor += 1;
        Some(v)
    }

    /// Record a live `Math.random()` value.
    pub fn push_random(&mut self, v: f64) {
        self.random.push(v);
        self.random_cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{Begin, Call, Outcome, Tape};
    use serde_json::json;

    fn cmd(id: &str) -> Call {
        Call::Command { id: id.into(), params: json!({}) }
    }

    #[test]
    fn replays_recorded_calls_and_detects_divergence() {
        let mut t = Tape::default();
        let Ok(Begin::Live(0)) = t.begin(cmd("a")) else { panic!("first call is live") };
        t.finish(0, Outcome::Return(json!(1)));
        t.rewind();
        let Ok(Begin::Replay(Outcome::Return(v))) = t.begin(cmd("a")) else { panic!("replayed") };
        assert_eq!(v, json!(1));
        t.rewind();
        assert!(t.begin(cmd("b")).is_err(), "a different call at a recorded position diverges");
    }

    #[test]
    fn clock_and_random_are_stable_across_attempts() {
        let mut t = Tape::default();
        t.push_random(0.25);
        let a = (t.clock(|| 5), 0.25);
        t.rewind();
        let b = (t.clock(|| 99), t.take_random().unwrap());
        assert_eq!(a, b);
        assert_eq!(t.clock(|| 7), 7, "past the recorded readings the live value is used");
    }
}
