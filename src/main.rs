mod driver;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Temporary demo entrypoint — replaced by the clap CLI (serve / run_flow).
    // Exercises the primitives end-to-end against a URL (env BROWSE_URL) and
    // writes a screenshot to SHOT_PATH so the run is observable.
    let base = std::env::var("BROWSE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8731/".to_string());
    let shot = std::env::var("SHOT_PATH").unwrap_or_else(|_| "/tmp/shot.png".to_string());

    let d = driver::Driver::launch().await?;

    // 1) navigate to the catalogue
    let settle = d.navigate(&base).await?;
    println!("[navigate] {base} -> settle={settle:?}");

    // 2) read the catalogue, click the first book link
    let first_title = d.get_text(".product_pod h3 a").await.unwrap_or_default();
    println!("[get_text] first book = {first_title:?}");
    d.click(".product_pod h3 a").await?;
    let settle = d.wait_settle(Some(8000)).await?;
    println!("[click+settle] -> {settle:?}");

    // 3) extract the datum straight from the DOM (never OCR what is text)
    let url = d.get_url().await?;
    let title = d.get_text("h1").await.unwrap_or_default();
    let price = d.get_text(".price_color").await.unwrap_or_default();
    let upc = d.get_attr("#upc", "id").await.ok().flatten();
    println!("[extract] url={url}");
    println!("[extract] title={title:?} price={price:?} upc_cell_id={upc:?}");

    // 4) evidence screenshot
    let png = d.screenshot(true).await?;
    let hash = blake3::hash(&png);
    std::fs::write(&shot, &png)?;
    println!("[evidence] screenshot {} bytes -> {shot}", png.len());
    println!("[evidence] blake3={}", hash.to_hex());

    d.close().await?;
    Ok(())
}
