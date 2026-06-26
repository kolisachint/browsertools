//! Discovery (Phase 2) — frozen signatures, not yet implemented.
//!
//! In P2 the parent drives the primitives to explore a task while the engine
//! *records* the trace; `compile` then turns a recording into a canonical flow
//! (action minimization, variable induction, selector synthesis, output-rule
//! learning). The MVP seeds flows by hand instead, to isolate the replay thesis.
#![allow(dead_code)]

use anyhow::Result;

use crate::flow::Flow;

/// Opaque id for an in-progress recording session.
pub type RecordingId = String;

/// Begin recording primitive calls against a goal (P2).
pub fn record_start(_goal: &str, _start_url: &str) -> Result<RecordingId> {
    anyhow::bail!("discovery (record_start) is a Phase 2 capability; not yet implemented")
}

/// Stop recording and return the recording id (P2).
pub fn record_stop(_id: &RecordingId) -> Result<RecordingId> {
    anyhow::bail!("discovery (record_stop) is a Phase 2 capability; not yet implemented")
}

/// Compile a recording into a canonical flow (P2).
pub fn compile(_id: &RecordingId) -> Result<Flow> {
    anyhow::bail!("discovery (compile) is a Phase 2 capability; not yet implemented")
}
