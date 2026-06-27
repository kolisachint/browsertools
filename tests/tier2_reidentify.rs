//! Tier 2 end-to-end: the parent-in-the-loop yield/resume contract.
//!
//! Drives a flow whose click selector has *drifted* (no longer matches the live
//! DOM). The engine must not fail: it suspends with `Outcome::NeedsParent`
//! carrying a `ReidentifyElement` request + a resume token, the parent (this
//! test, standing in for the LLM) hands back the corrected selector, and the run
//! resumes and completes deterministically — correct extraction, checkpoints
//! passed. No real model is involved; this exercises the contract path.
//!
//! Ignored by default (needs Chromium + this environment's setup):
//!
//!   cargo test --test tier2_reidentify -- --ignored --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Killer(Child);
impl Drop for Killer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A flow that navigates the catalogue then clicks a *wrong* selector (drift)
/// with no fallbacks, so the engine is forced to ask the parent to re-identify.
const DRIFTED_FLOW: &str = r#"{
  "id": "tier2_drift",
  "name": "drifted click triggers reidentify",
  "version": 1,
  "start_url": "{{base}}",
  "vars": [{ "key": "base", "required": true }],
  "steps": [
    { "id": "s01", "action": { "action": "navigate", "url": "{{base}}" }, "on_fail": "halt" },
    { "id": "s02", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s03", "action": { "action": "checkpoint",
        "asserts": [{ "kind": "element_present", "selector": ".product_pod h3 a" }] }, "on_fail": "halt" },
    { "id": "s04", "action": { "action": "click", "selector": ".totally-wrong-selector" }, "on_fail": "halt" },
    { "id": "s05", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s06", "action": { "action": "checkpoint",
        "asserts": [
          { "kind": "element_present", "selector": "h1" },
          { "kind": "element_present", "selector": ".price_color" },
          { "kind": "url_matches", "pattern": "catalogue/" }
        ] }, "on_fail": "halt" }
  ],
  "outputs": [
    { "key": "title", "source": { "from": "text", "selector": "h1" } },
    { "key": "price", "source": { "from": "text", "selector": ".price_color" } }
  ]
}"#;

#[test]
#[ignore = "needs Chromium + network setup; run with --ignored"]
fn drifted_selector_yields_then_resumes() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/site");
    let py = Command::new("python3")
        .args(["-m", "http.server", "8736", "--directory", fixtures])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start fixture server");
    let _py = Killer(py);
    std::thread::sleep(Duration::from_millis(800));

    let mut child = Command::new(env!("CARGO_BIN_EXE_browsertools"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));

    let read_json = |reader: &mut BufReader<_>| -> serde_json::Value {
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).expect("read line");
            assert!(n > 0, "serve closed stdout early");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            return serde_json::from_str(line).expect("valid json line");
        }
    };

    // ready event.
    let ready = read_json(&mut reader);
    assert_eq!(ready["event"], "ready", "expected ready, got {ready}");

    // Start the drifted flow.
    let flow: serde_json::Value = serde_json::from_str(DRIFTED_FLOW).unwrap();
    let start = serde_json::json!({
        "id": 1, "method": "flow_start",
        "params": { "flow": flow, "vars": { "base": "http://127.0.0.1:8736/" } }
    });
    writeln!(stdin, "{start}").unwrap();

    // The engine must pause and ask the parent to re-identify the element.
    let paused = read_json(&mut reader);
    assert!(
        paused.get("error").is_none(),
        "flow_start errored: {paused}"
    );
    let r = &paused["result"];
    assert_eq!(r["outcome"], "needs_parent", "expected pause, got {r}");
    assert_eq!(r["request"]["request"], "reidentify_element");
    assert!(
        r["request"]["screenshot_ref"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "request must carry a screenshot_ref: {r}"
    );
    let token = r["token"].as_str().expect("resume token").to_string();

    // Parent (playing the LLM) hands back the corrected selector.
    let resume = serde_json::json!({
        "id": 2, "method": "flow_resume",
        "params": {
            "token": token,
            "response": { "response": "element", "selector": ".product_pod h3 a" }
        }
    });
    writeln!(stdin, "{resume}").unwrap();

    // Now it must complete deterministically with correct extraction.
    let done = read_json(&mut reader);
    assert!(done.get("error").is_none(), "flow_resume errored: {done}");
    let r = &done["result"];
    assert_eq!(r["outcome"], "complete", "expected completion, got {r}");
    let result = &r["result"];
    assert_eq!(result["status"], "success", "run must succeed: {result}");
    assert_eq!(result["outputs"]["title"], "The Silent Compiler");
    assert_eq!(result["outputs"]["price"], "£42.10");
    assert_eq!(result["checkpoints_passed"], 2);

    writeln!(stdin, r#"{{"id":3,"method":"shutdown"}}"#).unwrap();
    let _ = child.wait();
    eprintln!("TIER2 OK: drift -> needs_parent(reidentify) -> resume -> deterministic success");
}
