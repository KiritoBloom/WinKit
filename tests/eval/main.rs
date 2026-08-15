//! WinKit failure-scenario evaluation suite (16 scenarios).
//!
//! Deterministic, fixture-backed evaluations of the developer-failure
//! workflows: machine pressure, workspace metadata, port ownership, HTTP
//! failures, browser evidence, managed-Chrome permissions, and the privacy
//! boundary. No developer machine state, no network beyond loopback, no
//! credentials, and no installed Chrome are required.
//!
//! Run with:
//!
//! ```text
//! cargo test --features mocks --test eval
//! ```
//!
//! See `tests/eval/README.md` for the scenario index and how to run the
//! suite (including the opt-in live Chrome test that covers real browser
//! startup, inspection, and cleanup).

mod helpers;
mod scenarios;
