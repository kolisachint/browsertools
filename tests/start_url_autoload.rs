//! Regression: a flow lands on its `start_url` before any step runs.
//!
//! The flow below has NO `navigate` step — only a checkpoint. If the engine did
//! not auto-navigate to `start_url`, the browser would sit on about:blank and the
//! checkpoint (and the `title` output) would fail. This pins the behavior that
//! `start_url` is honored, so callers no longer need a redundant first
//! `navigate` step.
//!
//! Ignored by default (needs Chromium + this environment's setup):
//!
//!   cargo test --test start_url_autoload -- --ignored --nocapture

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

/// No navigate step: success here proves the engine auto-loaded `start_url`.
const AUTOLOAD_FLOW: &str = r#"{
  "id": "start_url_autoload",
  "name": "auto-load start_url",
  "version": 1,
  "start_url": "{{base}}",
  "vars": [{ "key": "base", "required": true }],
  "steps": [
    { "id": "s01", "action": { "action": "wait_settle" }, "on_fail": "halt" },
    { "id": "s02", "action": { "action": "checkpoint",
        "asserts": [
          { "kind": "element_present", "selector": "h1" },
          { "kind": "text_present", "selector": "h1", "substr": "Fixture Bookstore" }
        ] }, "on_fail": "halt" }
  ],
  "outputs": [
    { "key": "title", "source": { "from": "text", "selector": "h1" } },
    { "key": "url", "source": { "from": "url" } }
  ]
}"#;

#[test]
#[ignore = "needs Chromium + network setup; run with --ignored"]
fn flow_auto_navigates_to_start_url_without_a_navigate_step() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/site");
    let py = Command::new("python3")
        .args(["-m", "http.server", "8741", "--directory", fixtures])
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

    let flow: serde_json::Value = serde_json::from_str(AUTOLOAD_FLOW).unwrap();
    let start = serde_json::json!({
        "id": 1, "method": "flow_start",
        "params": { "flow": flow, "vars": { "base": "http://127.0.0.1:8741/" } }
    });
    writeln!(stdin, "{start}").unwrap();

    let done = read_json(&mut reader);
    assert!(done.get("error").is_none(), "flow_start errored: {done}");
    let r = &done["result"];
    assert_eq!(r["outcome"], "complete", "expected completion, got {r}");
    let result = &r["result"];
    assert_eq!(
        result["status"], "success",
        "run must succeed without a navigate step: {result}"
    );
    assert_eq!(result["checkpoints_passed"], 1);
    // The output was read from the auto-loaded page, not about:blank.
    assert_eq!(result["outputs"]["title"], "Fixture Bookstore");
    assert_eq!(
        result["outputs"]["url"], "http://127.0.0.1:8741/",
        "url output must reflect the auto-loaded start_url"
    );

    writeln!(stdin, r#"{{"id":2,"method":"shutdown"}}"#).unwrap();
    let _ = child.wait();
    eprintln!("START_URL AUTOLOAD OK: no navigate step -> start_url loaded -> success");
}
