//! Deterministic diagnostic machinery for system diagnosis.
//!
//! `system::analyze_system` converts measured machine evidence into ranked
//! findings via explicit thresholds (`findings`) and status classification
//! (`health`). It is a pure, testable state machine — no LLM, no randomness,
//! no fabricated claims.

pub mod findings;
pub mod health;
pub mod system;
