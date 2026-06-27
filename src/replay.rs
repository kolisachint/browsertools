//! Replayer: execute a saved flow deterministically. Zero LLM calls.
//!
//! Resolves `{{vars}}`, runs each step, verifies checkpoints with DOM invariants,
//! extracts outputs straight from the DOM, and (optionally) writes a
//! tamper-evident evidence bundle.

use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::contract::{ParentRequest, ParentResponse, ResumeToken};
use crate::driver::Driver;
use crate::flow::{resolve, Action, Flow, Invariant, OnFail, Source};

#[derive(Debug, Serialize)]
pub struct StepTrace {
    pub id: String,
    pub action: String,
    pub status: String, // "ok" | "skipped" | "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub flow_id: String,
    pub run_id: String,
    pub status: String, // "success" | "failed"
    pub steps_executed: usize,
    pub steps_succeeded: usize,
    pub checkpoints_passed: usize,
    pub outputs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    pub duration_ms: u128,
    pub trace: Vec<StepTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_hash: Option<String>,
}

impl RunResult {
    pub fn succeeded(&self) -> bool {
        self.status == "success"
    }
}

fn now_run_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("run_{ms}")
}

/// Replay a flow. If `store_root` is provided, an evidence bundle is written to
/// `<store_root>/flows/<flow_id>/runs/<run_id>/`.
pub async fn run(
    driver: &Driver,
    flow: &Flow,
    vars: &BTreeMap<String, String>,
    store_root: Option<&Path>,
) -> Result<RunResult> {
    flow.check_vars(vars)?;
    let start = Instant::now();
    let run_id = now_run_id();

    let mut trace = Vec::new();
    let mut steps_executed = 0usize;
    let mut steps_succeeded = 0usize;
    let mut checkpoints_passed = 0usize;
    let mut failed_step: Option<String> = None;

    for step in &flow.steps {
        steps_executed += 1;
        let label = action_label(&step.action);
        let outcome = exec_step(driver, &step.action, vars, &mut checkpoints_passed).await;

        match outcome {
            Ok(()) => {
                steps_succeeded += 1;
                trace.push(StepTrace {
                    id: step.id.clone(),
                    action: label,
                    status: "ok".into(),
                    detail: None,
                });
            }
            Err(e) => {
                let detail = format!("{e:#}");
                match step.on_fail {
                    OnFail::Skip => {
                        trace.push(StepTrace {
                            id: step.id.clone(),
                            action: label,
                            status: "skipped".into(),
                            detail: Some(detail),
                        });
                    }
                    OnFail::Halt => {
                        trace.push(StepTrace {
                            id: step.id.clone(),
                            action: label,
                            status: "failed".into(),
                            detail: Some(detail),
                        });
                        failed_step = Some(step.id.clone());
                        break;
                    }
                }
            }
        }
    }

    let status = if failed_step.is_none() {
        "success"
    } else {
        "failed"
    };

    // Extract outputs (best effort; only meaningful on success).
    let mut outputs = BTreeMap::new();
    if failed_step.is_none() {
        for out in &flow.outputs {
            if let Ok(val) = extract(driver, &out.source).await {
                outputs.insert(out.key.clone(), val);
            }
        }
    }

    // Evidence bundle.
    let mut evidence_dir = None;
    let mut screenshot_hash = None;
    if let Some(root) = store_root {
        let (dir, hash) = persist_evidence(driver, root, &flow.id, &run_id, &outputs).await?;
        screenshot_hash = Some(hash);
        evidence_dir = Some(dir);
    }

    let result = RunResult {
        flow_id: flow.id.clone(),
        run_id,
        status: status.into(),
        steps_executed,
        steps_succeeded,
        checkpoints_passed,
        outputs,
        failed_step,
        duration_ms: start.elapsed().as_millis(),
        trace,
        evidence_dir: evidence_dir.clone(),
        screenshot_hash,
    };

    // Write the trace alongside evidence.
    if let Some(dir) = &evidence_dir {
        write_trace(dir, &result)?;
    }

    Ok(result)
}

/// Write the evidence bundle (full-page screenshot + extracted fields) for a
/// run, returning `(evidence_dir, screenshot_hash)`. Shared by the one-shot
/// [`run`] and the resumable [`FlowRun`] so both produce an identical bundle.
async fn persist_evidence(
    driver: &Driver,
    store_root: &Path,
    flow_id: &str,
    run_id: &str,
    outputs: &BTreeMap<String, String>,
) -> Result<(String, String)> {
    let dir = store_root
        .join("flows")
        .join(flow_id)
        .join("runs")
        .join(run_id);
    std::fs::create_dir_all(&dir)?;
    let png = driver.screenshot(true).await?;
    let hash = blake3::hash(&png).to_hex().to_string();
    std::fs::write(dir.join("evidence.png"), &png)?;
    std::fs::write(
        dir.join("extracted.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "fields": outputs,
            "screenshot_hash": hash,
        }))?,
    )?;
    Ok((dir.display().to_string(), hash))
}

/// Persist the full `RunResult` as `trace.json` next to the evidence.
fn write_trace(evidence_dir: &str, result: &RunResult) -> Result<()> {
    std::fs::write(
        PathBuf::from(evidence_dir).join("trace.json"),
        serde_json::to_vec_pretty(result)?,
    )?;
    Ok(())
}

async fn exec_step(
    d: &Driver,
    action: &Action,
    vars: &BTreeMap<String, String>,
    checkpoints_passed: &mut usize,
) -> Result<()> {
    match action {
        Action::Navigate { url } => {
            d.navigate(&resolve(url, vars)).await?;
        }
        Action::Click {
            selector,
            fallbacks,
        } => {
            click_with_fallbacks(d, selector, fallbacks).await?;
        }
        Action::Fill {
            selector,
            value_tpl,
        } => {
            d.fill(selector, &resolve(value_tpl, vars)).await?;
        }
        Action::Select {
            selector,
            value_tpl,
        } => {
            d.select(selector, &resolve(value_tpl, vars)).await?;
        }
        Action::WaitSettle => {
            d.wait_settle(None).await?;
        }
        Action::Checkpoint { asserts } => {
            for inv in asserts {
                verify(d, inv).await?;
            }
            *checkpoints_passed += 1;
        }
    }
    Ok(())
}

async fn click_with_fallbacks(d: &Driver, selector: &str, fallbacks: &[String]) -> Result<()> {
    if d.click(selector).await.is_ok() {
        return Ok(());
    }
    for fb in fallbacks {
        if d.click(fb).await.is_ok() {
            return Ok(());
        }
    }
    anyhow::bail!(
        "click failed for '{selector}' and {} fallback(s)",
        fallbacks.len()
    )
}

async fn verify(d: &Driver, inv: &Invariant) -> Result<()> {
    match inv {
        Invariant::ElementPresent { selector } => {
            if !d.exists(selector).await {
                anyhow::bail!("checkpoint: element not present: {selector}");
            }
        }
        Invariant::TextPresent { selector, substr } => {
            let hay = match selector {
                Some(sel) => d.get_text(sel).await.unwrap_or_default(),
                None => d.get_text("body").await.unwrap_or_default(),
            };
            if !hay.contains(substr) {
                anyhow::bail!("checkpoint: text {substr:?} not found");
            }
        }
        Invariant::UrlMatches { pattern } => {
            let url = d.get_url().await?;
            if !url.contains(pattern) {
                anyhow::bail!("checkpoint: url {url:?} does not contain {pattern:?}");
            }
        }
    }
    Ok(())
}

async fn extract(d: &Driver, source: &Source) -> Result<String> {
    Ok(match source {
        Source::Text { selector } => d.get_text(selector).await?,
        Source::Attr { selector, attr } => d.get_attr(selector, attr).await?.unwrap_or_default(),
        Source::Url => d.get_url().await?,
    })
}

// ---------------------------------------------------------------------------
// Tier 2: resumable, parent-in-the-loop execution.
//
// `FlowRun` runs a flow exactly like `run`, but instead of failing hard when it
// hits an ambiguity an LLM must resolve, it suspends with `Progress::Paused`,
// hands the parent a typed `ParentRequest`, and continues once the parent calls
// `resume`. The browser stays alive in the persistent `serve` session across the
// suspension, so no state needs serializing. The MVP fires exactly one variant —
// `ReidentifyElement` on selector drift — but every yield point routes through
// the same pause/resume machinery.
// ---------------------------------------------------------------------------

/// The yield point we paused on, so `resume` knows what corrective input applies.
enum Pending {
    /// A required click drifted; we asked the parent to re-identify the element.
    Reidentify { step_id: String, token: ResumeToken },
}

/// Corrective inputs the parent supplied via `resume`, keyed by step.
#[derive(Default)]
struct Overrides {
    /// step_id -> replacement selector for a drifted click.
    click_selector: BTreeMap<String, String>,
}

/// Outcome of advancing a [`FlowRun`].
pub enum Progress {
    /// The flow reached a terminal state (success or failure).
    Done(RunResult),
    /// The engine needs the parent to resolve something; the run is suspended.
    Paused {
        request: ParentRequest,
        token: ResumeToken,
    },
}

/// A resumable flow execution (Tier 2). Lives in the `serve` session.
pub struct FlowRun {
    flow: Flow,
    vars: BTreeMap<String, String>,
    store_root: Option<PathBuf>,
    run_id: String,
    start: Instant,
    cursor: usize,
    steps_executed: usize,
    steps_succeeded: usize,
    checkpoints_passed: usize,
    failed_step: Option<String>,
    trace: Vec<StepTrace>,
    overrides: Overrides,
    pending: Option<Pending>,
    token_seq: u64,
    /// Evidence bytes referenced by a yielded request (`screenshot_ref` -> PNG),
    /// so the parent can fetch what it is being asked to reason over while the
    /// run is suspended. Lives only as long as the run.
    resources: BTreeMap<String, Vec<u8>>,
}

impl FlowRun {
    /// Create a run. Validates required variables up front.
    pub fn new(
        flow: Flow,
        vars: BTreeMap<String, String>,
        store_root: Option<PathBuf>,
    ) -> Result<Self> {
        flow.check_vars(&vars)?;
        Ok(Self {
            flow,
            vars,
            store_root,
            run_id: now_run_id(),
            start: Instant::now(),
            cursor: 0,
            steps_executed: 0,
            steps_succeeded: 0,
            checkpoints_passed: 0,
            failed_step: None,
            trace: Vec::new(),
            overrides: Overrides::default(),
            pending: None,
            token_seq: 0,
            resources: BTreeMap::new(),
        })
    }

    fn next_token(&mut self) -> ResumeToken {
        self.token_seq += 1;
        format!("{}:{}", self.run_id, self.token_seq)
    }

    /// Fetch evidence bytes a yielded request referenced (e.g. its screenshot).
    pub fn resource(&self, id: &str) -> Option<&[u8]> {
        self.resources.get(id).map(|v| v.as_slice())
    }

    fn ok_step(&mut self, id: String, label: String) {
        self.steps_executed += 1;
        self.steps_succeeded += 1;
        self.trace.push(StepTrace {
            id,
            action: label,
            status: "ok".into(),
            detail: None,
        });
        self.cursor += 1;
    }

    /// Drive the flow forward until it completes or needs the parent.
    pub async fn advance(&mut self, d: &Driver) -> Result<Progress> {
        while self.cursor < self.flow.steps.len() {
            let step = self.flow.steps[self.cursor].clone();
            let label = action_label(&step.action);

            // Click is the only action that can yield (selector drift). Every
            // other action keeps the exact one-shot replay semantics.
            if let Action::Click {
                selector,
                fallbacks,
            } = &step.action
            {
                let sel = self
                    .overrides
                    .click_selector
                    .get(&step.id)
                    .cloned()
                    .unwrap_or_else(|| selector.clone());
                match click_with_fallbacks(d, &sel, fallbacks).await {
                    Ok(()) => self.ok_step(step.id.clone(), label),
                    Err(e) if step.on_fail == OnFail::Halt => {
                        // Required click drifted: suspend and ask the parent to
                        // re-identify the element rather than failing the run.
                        let png = d.screenshot(false).await?;
                        let screenshot_ref = blake3::hash(&png).to_hex().to_string();
                        // Retain the bytes so the parent can fetch them by ref
                        // (get_resource) while deciding how to re-identify.
                        self.resources.insert(screenshot_ref.clone(), png);
                        let token = self.next_token();
                        let description =
                            format!("{label}: selector {selector:?} no longer matches ({e:#})");
                        self.pending = Some(Pending::Reidentify {
                            step_id: step.id.clone(),
                            token: token.clone(),
                        });
                        return Ok(Progress::Paused {
                            request: ParentRequest::ReidentifyElement {
                                screenshot_ref,
                                description,
                            },
                            token,
                        });
                    }
                    Err(e) => {
                        // on_fail == Skip: record and move on, as one-shot does.
                        self.steps_executed += 1;
                        self.trace.push(StepTrace {
                            id: step.id.clone(),
                            action: label,
                            status: "skipped".into(),
                            detail: Some(format!("{e:#}")),
                        });
                        self.cursor += 1;
                    }
                }
                continue;
            }

            match exec_step(d, &step.action, &self.vars, &mut self.checkpoints_passed).await {
                Ok(()) => self.ok_step(step.id.clone(), label),
                Err(e) => {
                    let detail = format!("{e:#}");
                    self.steps_executed += 1;
                    match step.on_fail {
                        OnFail::Skip => {
                            self.trace.push(StepTrace {
                                id: step.id.clone(),
                                action: label,
                                status: "skipped".into(),
                                detail: Some(detail),
                            });
                            self.cursor += 1;
                        }
                        OnFail::Halt => {
                            self.trace.push(StepTrace {
                                id: step.id.clone(),
                                action: label,
                                status: "failed".into(),
                                detail: Some(detail),
                            });
                            self.failed_step = Some(step.id.clone());
                            break;
                        }
                    }
                }
            }
        }

        let result = self.finish(d).await?;
        Ok(Progress::Done(result))
    }

    /// Apply the parent's typed answer and continue from the suspended step.
    pub async fn resume(
        &mut self,
        d: &Driver,
        token: &str,
        response: ParentResponse,
    ) -> Result<Progress> {
        let pending = self
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("no pending parent request to resume"))?;
        match (pending, response) {
            (
                Pending::Reidentify {
                    step_id,
                    token: expect,
                },
                ParentResponse::Element { selector },
            ) => {
                if token != expect {
                    anyhow::bail!("resume token mismatch");
                }
                self.overrides.click_selector.insert(step_id, selector);
                // Cursor is unchanged: advance re-runs the step with the override.
                self.advance(d).await
            }
            (Pending::Reidentify { .. }, other) => {
                anyhow::bail!("reidentify expects an `element` response, got {other:?}")
            }
        }
    }

    /// Finalize: extract outputs, write the evidence bundle, build the result.
    async fn finish(&mut self, d: &Driver) -> Result<RunResult> {
        let status = if self.failed_step.is_none() {
            "success"
        } else {
            "failed"
        };

        let mut outputs = BTreeMap::new();
        if self.failed_step.is_none() {
            for out in &self.flow.outputs {
                if let Ok(val) = extract(d, &out.source).await {
                    outputs.insert(out.key.clone(), val);
                }
            }
        }

        let mut evidence_dir = None;
        let mut screenshot_hash = None;
        if let Some(root) = &self.store_root {
            let (dir, hash) =
                persist_evidence(d, root, &self.flow.id, &self.run_id, &outputs).await?;
            screenshot_hash = Some(hash);
            evidence_dir = Some(dir);
        }

        let result = RunResult {
            flow_id: self.flow.id.clone(),
            run_id: self.run_id.clone(),
            status: status.into(),
            steps_executed: self.steps_executed,
            steps_succeeded: self.steps_succeeded,
            checkpoints_passed: self.checkpoints_passed,
            outputs,
            failed_step: self.failed_step.clone(),
            duration_ms: self.start.elapsed().as_millis(),
            trace: std::mem::take(&mut self.trace),
            evidence_dir: evidence_dir.clone(),
            screenshot_hash,
        };
        if let Some(dir) = &evidence_dir {
            write_trace(dir, &result)?;
        }
        Ok(result)
    }
}

fn action_label(a: &Action) -> String {
    match a {
        Action::Navigate { url } => format!("navigate {url}"),
        Action::Click { selector, .. } => format!("click {selector}"),
        Action::Fill { selector, .. } => format!("fill {selector}"),
        Action::Select { selector, .. } => format!("select {selector}"),
        Action::WaitSettle => "wait_settle".into(),
        Action::Checkpoint { asserts } => format!("checkpoint ({} asserts)", asserts.len()),
    }
}
