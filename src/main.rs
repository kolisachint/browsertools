mod contract; // P2 yield contract (frozen, not yet exercised)
mod discover; // P2 discovery stubs (frozen signatures)
mod driver;
mod flow;
mod liveview;
mod observe;
mod replay;
mod serve;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "browsertools",
    about = "Deterministic browser engine (no LLM in-process)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Long-running native protocol: newline-delimited JSON on stdin/stdout.
    Serve,
    /// Replay a saved flow once (deterministic, zero LLM) and write evidence.
    RunFlow {
        /// Path to the flow JSON file.
        #[arg(long)]
        flow: PathBuf,
        /// Variable assignment, repeatable: --var key=value
        #[arg(long = "var", value_parser = parse_kv)]
        vars: Vec<(String, String)>,
        /// Evidence store root (a `.hoocode` tree is created under it).
        #[arg(long, default_value = ".hoocode")]
        store: PathBuf,
    },
    /// One-shot demo exercising the primitives against a fixture site.
    Demo,
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got '{s}'"))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve => serve::run().await,
        Cmd::RunFlow { flow, vars, store } => run_flow(flow, vars, store).await,
        Cmd::Demo => demo().await,
    }
}

/// `run_flow` one-shot: load a flow, replay it, print the RunResult JSON.
/// Exits non-zero if the flow did not succeed.
async fn run_flow(flow_path: PathBuf, vars: Vec<(String, String)>, store: PathBuf) -> Result<()> {
    let flow = flow::Flow::load(&flow_path)?;
    let vars: BTreeMap<String, String> = vars.into_iter().collect();

    let d = driver::Driver::launch().await?;
    let result = replay::run(&d, &flow, &vars, Some(store.as_path())).await?;
    d.close().await.ok();

    println!("{}", serde_json::to_string_pretty(&result)?);
    if !result.succeeded() {
        std::process::exit(1);
    }
    Ok(())
}

async fn pause(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Demo: navigate a fixture catalogue, open the first book, extract + observe.
async fn demo() -> Result<()> {
    let base = std::env::var("BROWSE_URL").unwrap_or_else(|_| "http://127.0.0.1:8731/".to_string());
    let shot = std::env::var("SHOT_PATH").unwrap_or_else(|_| "/tmp/shot.png".to_string());

    let d = driver::Driver::launch().await?;

    let settle = d.navigate(&base).await?;
    println!("[navigate] {base} -> settle={settle:?}");

    let first_title = d.get_text(".product_pod h3 a").await.unwrap_or_default();
    println!("[get_text] first book = {first_title:?}");
    d.click(".product_pod h3 a").await?;
    let settle = d.wait_settle(Some(8000)).await?;
    println!("[click+settle] -> {settle:?}");

    let url = d.get_url().await?;
    let title = d.get_text("h1").await.unwrap_or_default();
    let price = d.get_text(".price_color").await.unwrap_or_default();
    println!("[extract] url={url}");
    println!("[extract] title={title:?} price={price:?}");

    let obs = d.observe().await?;
    println!("[observe] state_signature={}", obs.state_signature);
    println!(
        "[observe] title={:?} landmarks={} inputs={} headings={:?} error={}",
        obs.title,
        obs.landmarks.len(),
        obs.inputs.len(),
        obs.text_blocks,
        obs.has_error_region
    );

    let png = d.screenshot(true).await?;
    let hash = blake3::hash(&png);
    std::fs::write(&shot, &png)?;
    println!(
        "[evidence] screenshot {} bytes -> {shot} (blake3={})",
        png.len(),
        hash.to_hex()
    );

    pause(100).await;
    d.close().await?;
    Ok(())
}
