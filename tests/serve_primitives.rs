//! Integration test for `serve` mode: drive a stateful primitive sequence over
//! the stdio JSON protocol against the local fixture site.
//!
//! Ignored by default because it needs Chromium plus this environment's proxy /
//! NSS trust setup. Run explicitly:
//!
//!   cargo test --test serve_primitives -- --ignored
//!
//! The pure observation/signature logic is covered by fast unit tests in
//! `src/observe.rs`; this exercises the real browser + protocol path.

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

#[test]
#[ignore = "needs Chromium + network setup; run with --ignored"]
fn serve_primitive_sequence() {
    // Serve the fixture on loopback.
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/site");
    let py = Command::new("python3")
        .args(["-m", "http.server", "8732", "--directory", fixtures])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start fixture server");
    let _py = Killer(py);
    std::thread::sleep(Duration::from_millis(800));

    // Launch the engine in serve mode.
    let mut child = Command::new(env!("CARGO_BIN_EXE_browsertools"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let requests = [
        r#"{"id":1,"method":"navigate","params":{"url":"http://127.0.0.1:8732/"}}"#,
        r#"{"id":2,"method":"get_text","params":{"selector":".product_pod h3 a"}}"#,
        r#"{"id":3,"method":"click","params":{"selector":".product_pod h3 a"}}"#,
        r#"{"id":4,"method":"wait_settle","params":{"timeout_ms":8000}}"#,
        r#"{"id":5,"method":"get_text","params":{"selector":"h1"}}"#,
        r#"{"id":6,"method":"observe"}"#,
        r#"{"id":7,"method":"shutdown"}"#,
    ];
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for r in requests {
            writeln!(stdin, "{r}").expect("write request");
        }
    }

    let stdout = child.stdout.take().expect("stdout");
    let mut by_id: std::collections::HashMap<i64, serde_json::Value> = Default::default();
    let mut saw_ready = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read line");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
        if v.get("event").and_then(|e| e.as_str()) == Some("ready") {
            saw_ready = true;
            continue;
        }
        if let Some(id) = v.get("id").and_then(|i| i.as_i64()) {
            assert!(v.get("error").is_none(), "request {id} errored: {v}");
            by_id.insert(id, v);
        }
    }
    let _ = child.wait();

    assert!(saw_ready, "serve must announce readiness");

    // Navigation settled.
    assert_eq!(by_id[&1]["result"]["load"], serde_json::json!(true));
    // Catalogue link text.
    assert_eq!(by_id[&2]["result"]["text"], "The Silent Compiler");
    // After click, the detail heading.
    assert_eq!(by_id[&5]["result"]["text"], "The Silent Compiler");
    // Observation carries a non-empty deterministic signature.
    let sig = by_id[&6]["result"]["state_signature"]
        .as_str()
        .expect("signature string");
    assert_eq!(sig.len(), 64, "blake3 hex signature");
}
