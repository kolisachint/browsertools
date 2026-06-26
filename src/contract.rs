//! The yield contract between this engine and the parent (hoocode).
//!
//! Frozen now, exercised in Phase 2. The engine runs deterministically until it
//! hits something only an LLM can resolve, then returns `Outcome::NeedsParent`
//! with a typed request and a resume token; the parent fulfils it with its own
//! LLM and calls `resume`. Every place the old design would have made a vision
//! call is exactly one `ParentRequest` variant.
//!
//! On the MVP target no `NeedsParent` ever fires, so these types are defined but
//! not yet produced. Kept here so the contract is stable before P2 wiring.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::observe::{Observation, ResourceId};

/// Opaque handle the parent passes back to `resume` a paused run.
pub type ResumeToken = String;

/// Reference to a written evidence bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub flow_id: String,
    pub run_id: String,
    pub dir: String,
    pub screenshot_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailKind {
    ActionFailed,
    CheckpointFailed,
    SelectorDrift,
    LayoutDrift,
    Blocked, // anti-bot / captcha terminal state
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Done {
        evidence: EvidenceRef,
    },
    Failed {
        step_id: String,
        kind: FailKind,
        detail: String,
    },
    NeedsParent {
        request: ParentRequest,
        token: ResumeToken,
    },
}

/// A judgment the engine needs the parent's LLM to make. The screenshot is
/// referenced (not embedded); the parent fetches bytes by `ResourceId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum ParentRequest {
    ClassifyState {
        screenshot_ref: ResourceId,
        observation: Observation,
    },
    VerifyVisual {
        screenshot_ref: ResourceId,
        expected_state: String,
    },
    ExtractSemantic {
        screenshot_ref: ResourceId,
        fields: Vec<String>,
    },
    DecideNextAction {
        screenshot_ref: ResourceId,
        observation: Observation,
        goal: String,
    },
    ReidentifyElement {
        screenshot_ref: ResourceId,
        description: String,
    },
}

/// The parent's answer to a `ParentRequest`, handed back via `resume`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum ParentResponse {
    State { state: String },
    Verified { passed: bool },
    Extracted { fields: std::collections::BTreeMap<String, String> },
    NextAction { action: serde_json::Value },
    Element { selector: String },
}
