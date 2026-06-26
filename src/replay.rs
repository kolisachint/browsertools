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
        let dir = root.join("flows").join(&flow.id).join("runs").join(&run_id);
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
        screenshot_hash = Some(hash);
        evidence_dir = Some(dir.display().to_string());
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
        std::fs::write(
            PathBuf::from(dir).join("trace.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
    }

    Ok(result)
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
