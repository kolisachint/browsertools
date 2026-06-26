mod driver;

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;

async fn pause(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

#[tokio::main]
async fn main() -> Result<()> {
    // Temporary demo entrypoint — replaced by the clap CLI (serve / run_flow).
    // Exercises the primitives against BROWSE_URL. When FRAMES_DIR is set, a
    // live CDP screencast is recorded into it (the "see it live" mechanism).
    let base = std::env::var("BROWSE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8731/".to_string());
    let shot = std::env::var("SHOT_PATH").unwrap_or_else(|_| "/tmp/shot.png".to_string());
    let frames_dir = std::env::var("FRAMES_DIR").ok().map(PathBuf::from);

    let d = driver::Driver::launch().await?;

    // 1) navigate to the catalogue
    let settle = d.navigate(&base).await?;
    println!("[navigate] {base} -> settle={settle:?}");

    // Start live screencast now that the first paint exists.
    let cast = match &frames_dir {
        Some(dir) => {
            // Interval capture (~10 fps) for a smooth headless "live" video.
            let h = d.start_interval_capture(dir.clone(), 100).await?;
            println!("[live] recording frames -> {}", dir.display());
            Some(h)
        }
        None => None,
    };

    pause(700).await;
    d.scroll(0, 220).await.ok();
    pause(500).await;
    d.scroll(0, -220).await.ok();
    pause(400).await;

    // 2) read the catalogue, click the first book link
    let first_title = d.get_text(".product_pod h3 a").await.unwrap_or_default();
    println!("[get_text] first book = {first_title:?}");
    d.click(".product_pod h3 a").await?;
    let settle = d.wait_settle(Some(8000)).await?;
    println!("[click+settle] -> {settle:?}");
    pause(700).await;
    d.scroll(0, 160).await.ok();
    pause(600).await;

    // 3) extract the datum straight from the DOM (never OCR what is text)
    let url = d.get_url().await?;
    let title = d.get_text("h1").await.unwrap_or_default();
    let price = d.get_text(".price_color").await.unwrap_or_default();
    println!("[extract] url={url}");
    println!("[extract] title={title:?} price={price:?}");
    pause(500).await;

    // 4) evidence screenshot
    let png = d.screenshot(true).await?;
    let hash = blake3::hash(&png);
    std::fs::write(&shot, &png)?;
    println!("[evidence] screenshot {} bytes -> {shot}", png.len());
    println!("[evidence] blake3={}", hash.to_hex());

    if let Some(h) = cast {
        let n = h.stop().await?;
        println!("[screencast] captured {n} frames");
    }

    d.close().await?;
    Ok(())
}
