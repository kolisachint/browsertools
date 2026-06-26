mod driver;
mod liveview;
mod observe;
mod serve;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "browsertools", about = "Deterministic browser engine (no LLM in-process)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Long-running native protocol: newline-delimited JSON on stdin/stdout.
    Serve,
    /// One-shot demo exercising the primitives against a fixture site.
    Demo,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve => serve::run().await,
        Cmd::Demo => demo().await,
    }
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
    println!("[evidence] screenshot {} bytes -> {shot} (blake3={})", png.len(), hash.to_hex());

    pause(100).await;
    d.close().await?;
    Ok(())
}
