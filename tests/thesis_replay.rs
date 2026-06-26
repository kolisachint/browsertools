//! Thesis test: deterministic replay survives across runs.
//!
//! Cold-launches `run-flow` N=20 times against the local fixture and asserts:
//!   - every run succeeds,
//!   - extraction is correct every run,
//!   - the evidence screenshot hash is identical across all runs (full
//!     determinism — the page rendered byte-for-byte the same each time).
//!
//! Zero LLM calls are involved at any point. Ignored by default (needs Chromium
//! + this environment's proxy/NSS setup); run with:
//!
//!   cargo test --test thesis_replay -- --ignored --nocapture

use std::collections::HashSet;
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
fn replay_survives_twenty_runs() {
    const N: usize = 20;
    let manifest = env!("CARGO_MANIFEST_DIR");
    let fixtures = format!("{manifest}/fixtures/site");
    let flow = format!("{manifest}/fixtures/flows/bookstore_first_book.json");
    let store = std::env::temp_dir().join("bt_thesis_store");
    let _ = std::fs::remove_dir_all(&store);

    let py = Command::new("python3")
        .args(["-m", "http.server", "8734", "--directory", &fixtures])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start fixture server");
    let _py = Killer(py);
    std::thread::sleep(Duration::from_millis(800));

    let mut hashes: HashSet<String> = HashSet::new();
    let mut successes = 0usize;

    for i in 0..N {
        let out = Command::new(env!("CARGO_BIN_EXE_browsertools"))
            .args([
                "run-flow",
                "--flow",
                &flow,
                "--var",
                "base=http://127.0.0.1:8734/",
                "--store",
                store.to_str().unwrap(),
            ])
            .stderr(Stdio::null())
            .output()
            .expect("spawn run-flow");

        assert!(out.status.success(), "run {i} exited non-zero");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("run-flow stdout is JSON");

        assert_eq!(v["status"], "success", "run {i} not success: {v}");
        assert_eq!(
            v["outputs"]["title"], "The Silent Compiler",
            "run {i} title"
        );
        assert_eq!(v["outputs"]["price"], "£42.10", "run {i} price");
        assert_eq!(v["checkpoints_passed"], 2, "run {i} checkpoints");

        hashes.insert(v["screenshot_hash"].as_str().unwrap().to_string());
        successes += 1;
        eprintln!(
            "run {:02}/{N}: ok  sig={}",
            i + 1,
            &v["screenshot_hash"].as_str().unwrap()[..16]
        );
    }

    assert_eq!(successes, N, "all {N} runs must succeed");
    assert_eq!(
        hashes.len(),
        1,
        "evidence screenshot must be byte-identical across all runs (got {} distinct)",
        hashes.len()
    );
    eprintln!("THESIS OK: {N}/{N} deterministic replays, single evidence hash");
}
