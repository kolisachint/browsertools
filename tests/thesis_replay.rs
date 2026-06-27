//! Thesis test: deterministic replay survives across runs.
//!
//! Cold-launches `run-flow` N=20 times against the local fixture and asserts
//! the thesis — *extraction* determinism:
//!   - every run succeeds,
//!   - extraction (title/price) is correct and identical every run,
//!   - every checkpoint passes every run.
//!
//! It also *reports* a bonus signal — whether the evidence screenshot is
//! byte-identical across all runs (rendering determinism). That is not part of
//! the thesis: a CI runner with different fonts/AA can produce >1 distinct hash
//! without falsifying deterministic replay. Set THESIS_STRICT_PIXELS=1 to
//! promote it back to a hard assertion.
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

    // Thesis is proven by the per-run extraction/checkpoint asserts above.
    // Pixel-identity is a reported bonus, enforced only under an opt-in flag.
    let distinct = hashes.len();
    eprintln!(
        "THESIS OK: {N}/{N} deterministic replays; distinct evidence hashes = {distinct} \
         (1 = also pixel-identical)"
    );
    if std::env::var("THESIS_STRICT_PIXELS").as_deref() == Ok("1") {
        assert_eq!(
            distinct, 1,
            "THESIS_STRICT_PIXELS=1: evidence screenshot must be byte-identical \
             across all runs (got {distinct} distinct)"
        );
    }
}
