//! Tier 2 end-to-end: a delegated decision point.
//!
//! A flow reaches a `decide` step — the engine has no recorded action for the
//! state and asks the parent what to do. The engine suspends with a
//! `DecideNextAction` request carrying the page `observation` + a screenshot
//! ref; the parent (this test, standing in for the LLM) returns a concrete
//! primitive action (`next_action`), which the engine executes before resuming
//! to deterministic completion.
//!
//! Ignored by default (needs Chromium + this environment's setup):
//!
//!   cargo test --test tier2_decide -- --ignored --nocapture

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

/// Navigate the catalogue, then delegate "open the first book" to the parent.
const DECIDE_FLOW: &str = r#"{
  "id": "tier2_decide",
  "name": "delegated decision point",
  "version": 1,
  "start_url": "{{base}}",
  "vars": [{ "key": "base", "required": true }],
  "steps": [
    { "id": "s01", "action": { "action": "navigate", "url": "{{base}}" }, "on_fail": "halt" },
    { "id": "s02", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s03", "action": { "action": "decide", "goal": "open the first book's detail page" }, "on_fail": "halt" },
    { "id": "s04", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s05", "action": { "action": "checkpoint",
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
fn decide_step_yields_then_resumes_with_action() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/site");
    let py = Command::new("python3")
        .args(["-m", "http.server", "8737", "--directory", fixtures])
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

    let ready = read_json(&mut reader);
    assert_eq!(ready["event"], "ready", "expected ready, got {ready}");

    let flow: serde_json::Value = serde_json::from_str(DECIDE_FLOW).unwrap();
    let start = serde_json::json!({
        "id": 1, "method": "flow_start",
        "params": { "flow": flow, "vars": { "base": "http://127.0.0.1:8737/" } }
    });
    writeln!(stdin, "{start}").unwrap();

    // The engine must pause at the decide step and hand over an observation.
    let paused = read_json(&mut reader);
    assert!(
        paused.get("error").is_none(),
        "flow_start errored: {paused}"
    );
    let r = &paused["result"];
    assert_eq!(r["outcome"], "needs_parent", "expected pause, got {r}");
    assert_eq!(r["request"]["request"], "decide_next_action");
    assert_eq!(
        r["request"]["goal"], "open the first book's detail page",
        "goal must round-trip"
    );
    // The observation carries deterministic facts the parent reasons over.
    let sig = r["request"]["observation"]["state_signature"]
        .as_str()
        .expect("observation.state_signature");
    assert_eq!(sig.len(), 64, "blake3 hex signature");
    let token = r["token"].as_str().expect("resume token").to_string();

    // Parent decides: click the first book. Engine executes it, then continues.
    let resume = serde_json::json!({
        "id": 2, "method": "flow_resume",
        "params": {
            "token": token,
            "response": { "response": "next_action",
                "action": { "action": "click", "selector": ".product_pod h3 a" } }
        }
    });
    writeln!(stdin, "{resume}").unwrap();

    let done = read_json(&mut reader);
    assert!(done.get("error").is_none(), "flow_resume errored: {done}");
    let r = &done["result"];
    assert_eq!(r["outcome"], "complete", "expected completion, got {r}");
    let result = &r["result"];
    assert_eq!(result["status"], "success", "run must succeed: {result}");
    assert_eq!(result["outputs"]["title"], "The Silent Compiler");
    assert_eq!(result["outputs"]["price"], "£42.10");
    assert_eq!(result["checkpoints_passed"], 1);

    writeln!(stdin, r#"{{"id":3,"method":"shutdown"}}"#).unwrap();
    let _ = child.wait();
    eprintln!("TIER2 DECIDE OK: decide -> needs_parent -> next_action -> deterministic success");
}
