//! Tier 2 end-to-end: the judgment variants (`classify`, `verify_visual`,
//! `extract_semantic`), exercised back-to-back in a single suspended run.
//!
//! These yield points hand the parent something only an LLM can judge from
//! pixels. The test stands in for that LLM with canned answers — it proves the
//! pause/resume *plumbing* (request shapes, tokens, chained suspensions, and
//! that a semantic field is merged into the outputs); the judgments themselves
//! are validated against real pages later.
//!
//! Ignored by default (needs Chromium + this environment's setup):
//!
//!   cargo test --test tier2_judgment -- --ignored --nocapture

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

const JUDGMENT_FLOW: &str = r#"{
  "id": "tier2_judgment",
  "name": "classify + verify_visual + extract_semantic",
  "version": 1,
  "start_url": "{{base}}",
  "vars": [{ "key": "base", "required": true }],
  "steps": [
    { "id": "s01", "action": { "action": "navigate", "url": "{{base}}" }, "on_fail": "halt" },
    { "id": "s02", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s03", "action": { "action": "classify" }, "on_fail": "halt" },
    { "id": "s04", "action": { "action": "click", "selector": ".product_pod h3 a" }, "on_fail": "halt" },
    { "id": "s05", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s06", "action": { "action": "verify_visual", "expected_state": "book detail page" }, "on_fail": "halt" },
    { "id": "s07", "action": { "action": "extract_semantic", "fields": ["rating"] }, "on_fail": "halt" },
    { "id": "s08", "action": { "action": "checkpoint",
        "asserts": [
          { "kind": "element_present", "selector": "h1" },
          { "kind": "element_present", "selector": ".price_color" }
        ] }, "on_fail": "halt" }
  ],
  "outputs": [
    { "key": "title", "source": { "from": "text", "selector": "h1" } },
    { "key": "price", "source": { "from": "text", "selector": ".price_color" } }
  ]
}"#;

#[test]
#[ignore = "needs Chromium + network setup; run with --ignored"]
fn judgment_variants_chain_through_one_run() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/site");
    let py = Command::new("python3")
        .args(["-m", "http.server", "8738", "--directory", fixtures])
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

    // Pull the next response, assert it paused with `kind`, return the token.
    let expect_pause = |v: &serde_json::Value, kind: &str| -> String {
        assert!(v.get("error").is_none(), "request errored: {v}");
        let r = &v["result"];
        assert_eq!(r["outcome"], "needs_parent", "expected pause, got {r}");
        assert_eq!(r["request"]["request"], kind, "wrong request kind: {r}");
        assert!(
            r["request"]["screenshot_ref"]
                .as_str()
                .is_some_and(|s| s.len() == 64),
            "every judgment request carries a screenshot_ref: {r}"
        );
        r["token"].as_str().expect("token").to_string()
    };

    let resume = |stdin: &mut std::process::ChildStdin,
                  id: i64,
                  token: &str,
                  response: serde_json::Value| {
        let msg = serde_json::json!({
            "id": id, "method": "flow_resume",
            "params": { "token": token, "response": response }
        });
        writeln!(stdin, "{msg}").unwrap();
    };

    assert_eq!(read_json(&mut reader)["event"], "ready");

    let flow: serde_json::Value = serde_json::from_str(JUDGMENT_FLOW).unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "id": 1, "method": "flow_start",
            "params": { "flow": flow, "vars": { "base": "http://127.0.0.1:8738/" } }
        })
    )
    .unwrap();

    // 1. classify — parent labels the page.
    let t = expect_pause(&read_json(&mut reader), "classify_state");
    resume(
        &mut stdin,
        2,
        &t,
        serde_json::json!({"response": "state", "state": "catalogue"}),
    );

    // 2. verify_visual — parent confirms the detail page looks right.
    let t = expect_pause(&read_json(&mut reader), "verify_visual");
    resume(
        &mut stdin,
        3,
        &t,
        serde_json::json!({"response": "verified", "passed": true}),
    );

    // 3. extract_semantic — parent reads a pixel-only field.
    let t = expect_pause(&read_json(&mut reader), "extract_semantic");
    resume(
        &mut stdin,
        4,
        &t,
        serde_json::json!({"response": "extracted", "fields": {"rating": "4 stars"}}),
    );

    // Run completes; the semantic field must be merged into the outputs.
    let done = read_json(&mut reader);
    assert!(done.get("error").is_none(), "final resume errored: {done}");
    let r = &done["result"];
    assert_eq!(r["outcome"], "complete", "expected completion, got {r}");
    let result = &r["result"];
    assert_eq!(result["status"], "success", "run must succeed: {result}");
    assert_eq!(result["outputs"]["title"], "The Silent Compiler");
    assert_eq!(result["outputs"]["price"], "£42.10");
    assert_eq!(
        result["outputs"]["rating"], "4 stars",
        "semantic field merged"
    );
    assert_eq!(result["checkpoints_passed"], 1);

    writeln!(stdin, r#"{{"id":9,"method":"shutdown"}}"#).unwrap();
    let _ = child.wait();
    eprintln!("TIER2 JUDGMENT OK: classify -> verify_visual -> extract_semantic -> success");
}
