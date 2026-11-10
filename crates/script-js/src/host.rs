//! What a run can reach outside the engine.

use std::fmt;

use serde_json::Value;

/// A failed host call. Shown to the script as a catchable `Error` with this message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// The application side of a run. Calls arrive only from the first attempt that reaches them;
/// replays are answered from the tape.
pub trait Host {
    /// Run a session command by id (the same ids and params as automation).
    ///
    /// # Errors
    /// The command's error.
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, HostError>;

    /// A line of script output (`$.writeln`).
    fn write_line(&mut self, line: &str);
}
