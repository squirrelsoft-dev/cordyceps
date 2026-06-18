//! `csv-examiner` — the scorer for the Cordyceps HillClimber proof (v0.0.1).
//!
//! This crate intentionally holds NO parsing logic. Its only job is to own the
//! hidden held-out RFC 4180 test suite in `tests/heldout.rs`, which scores the
//! `csv_task::parse_csv` implementation the proposing agent writes.
//!
//! Separation of powers (see docs/plans/v0.0.1-hillclimber-proof.md §"fitness
//! function"): this crate lives OUTSIDE the agent's write scope. The agent only
//! edits `csv-task/`; it never sees, runs, or can modify the held-out tests
//! here. The examiner runs `cargo test` in this crate against whatever
//! `csv-task` currently builds and reports the passing fraction k/N.
