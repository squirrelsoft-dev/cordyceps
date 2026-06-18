//! `csv-task` — the scored task for the Cordyceps HillClimber proof (v0.0.1).
//!
//! The single public entry point, [`parse_csv`], is deliberately left
//! `unimplemented!()`. Making it correct against the RFC 4180 test suite is the
//! job handed to the proposing agent; the fraction of tests it turns green is
//! the fitness number the examiner reports. Do not implement the body here.

use std::error::Error;
use std::fmt;

/// Error returned when CSV input cannot be parsed per RFC 4180.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsvError {
    /// A field opened with a double-quote that was never closed before the end
    /// of input (RFC 4180 §2.5).
    UnterminatedQuote,
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CsvError::UnterminatedQuote => write!(f, "unterminated quoted field"),
        }
    }
}

impl Error for CsvError {}

/// Parse RFC 4180 CSV text into rows of fields.
pub fn parse_csv(input: &str) -> Result<Vec<Vec<String>>, CsvError> {
    // Intentionally unimplemented: this is the failing target the HillClimber
    // is asked to make green. Do not implement.
    let _ = input;
    unimplemented!()
}
