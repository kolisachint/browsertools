//! Flow file schema (MVP-minimal) + `{{var}}` resolution.
//!
//! A flow is the canonical, deterministic procedure produced by discovery and
//! executed by the replayer with zero LLM calls. Verification is DOM-invariant
//! only; output extraction reads the datum straight from the DOM.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub start_url: String,
    #[serde(default)]
    pub vars: Vec<VarSpec>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarSpec {
    pub key: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub example: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub action: Action,
    #[serde(default)]
    pub on_fail: OnFail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Navigate {
        url: String,
    },
    Click {
        selector: String,
        #[serde(default)]
        fallbacks: Vec<String>,
    },
    Fill {
        selector: String,
        value_tpl: String,
    },
    Select {
        selector: String,
        value_tpl: String,
    },
    WaitSettle,
    Checkpoint {
        asserts: Vec<Invariant>,
    },
    /// A delegated decision point (Tier 2): the engine has no recorded action
    /// for this state and asks the parent what to do next. Only resolvable in the
    /// parent-in-the-loop `serve` path; one-shot replay treats it as a failure.
    Decide {
        goal: String,
    },
    /// Ask the parent to classify the current page state (Tier 2).
    Classify,
    /// Ask the parent to confirm the page visually matches `expected_state`
    /// (Tier 2). A `false` verdict fails the run, like a checkpoint.
    VerifyVisual {
        expected_state: String,
    },
    /// Ask the parent to read `fields` that live in pixels, not the text DOM
    /// (Tier 2). Returned values are merged into the run outputs.
    ExtractSemantic {
        fields: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Invariant {
    ElementPresent {
        selector: String,
    },
    TextPresent {
        #[serde(default)]
        selector: Option<String>,
        substr: String,
    },
    UrlMatches {
        pattern: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Source {
    Text { selector: String },
    Attr { selector: String, attr: String },
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSpec {
    pub key: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OnFail {
    #[default]
    Halt,
    Skip,
}

impl Flow {
    pub fn load(path: impl AsRef<Path>) -> Result<Flow> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).with_context(|| format!("read flow file {}", path.display()))?;
        let flow: Flow = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse flow file {}", path.display()))?;
        Ok(flow)
    }

    /// Check that every required variable is provided.
    pub fn check_vars(&self, vars: &BTreeMap<String, String>) -> Result<()> {
        for v in &self.vars {
            if v.required && !vars.contains_key(&v.key) {
                anyhow::bail!("missing required variable '{}'", v.key);
            }
        }
        Ok(())
    }
}

/// Replace every `{{key}}` occurrence with its value. Unknown keys are left as-is
/// so a missing-variable error surfaces at use rather than silently blanking.
pub fn resolve(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, val) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), val);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_vars() {
        let mut vars = BTreeMap::new();
        vars.insert("base".to_string(), "http://x/".to_string());
        vars.insert("q".to_string(), "rust".to_string());
        assert_eq!(
            resolve("{{base}}search?q={{q}}", &vars),
            "http://x/search?q=rust"
        );
    }

    #[test]
    fn parses_action_variants() {
        let j = r##"{"action":"click","selector":"#go"}"##;
        let a: Action = serde_json::from_str(j).unwrap();
        matches!(a, Action::Click { .. }).then_some(()).unwrap();

        let w: Action = serde_json::from_str(r#"{"action":"wait_settle"}"#).unwrap();
        matches!(w, Action::WaitSettle).then_some(()).unwrap();
    }

    #[test]
    fn check_vars_flags_missing_required() {
        let flow: Flow = serde_json::from_str(
            r#"{"id":"f","name":"f","version":1,"start_url":"{{base}}",
                "vars":[{"key":"base","required":true}],
                "steps":[],"outputs":[]}"#,
        )
        .unwrap();
        assert!(flow.check_vars(&BTreeMap::new()).is_err());
        let mut v = BTreeMap::new();
        v.insert("base".to_string(), "http://x/".to_string());
        assert!(flow.check_vars(&v).is_ok());
    }
}
