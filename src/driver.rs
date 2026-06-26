//! Browser driver: a thin, deterministic wrapper over chromiumoxide (raw CDP).
//!
//! This is the "dumb hands" layer. It drives Chromium and returns raw facts
//! (bytes, strings, structured DOM observations). It makes no decisions and no
//! LLM calls. Higher layers (replayer, serve) interpret results.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat,
    StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Path to the pre-installed Chromium in this environment.
const CHROMIUM_PATH: &str = "/opt/pw-browsers/chromium";

/// Result of waiting for the page to settle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettleInfo {
    pub load: bool,
    pub network_idle: bool,
    pub timed_out: bool,
}

/// Handle to a running screencast. Drop or call `stop()` to end it.
pub struct ScreencastHandle {
    page: Page,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<usize>,
}

impl ScreencastHandle {
    /// Stop the screencast and return the number of frames captured.
    pub async fn stop(mut self) -> Result<usize> {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        let _ = self.page.execute(StopScreencastParams::default()).await;
        Ok(self.task.await.unwrap_or(0))
    }
}

/// A live browser session. Owns one browser + one page (tab).
///
/// The chromiumoxide handler future must be polled for the connection to make
/// progress, so we spawn it onto the tokio runtime and keep the join handle.
pub struct Driver {
    browser: Browser,
    page: Page,
    _handler: tokio::task::JoinHandle<()>,
}

impl Driver {
    /// Launch a headless Chromium and open a blank page.
    ///
    /// Outbound HTTPS in this environment goes through the agent proxy, and TLS is
    /// re-terminated there, so Chromium must (a) route through the proxy and
    /// (b) trust the proxy CA via the NSS store at `$HOME/.pki/nssdb`.
    pub async fn launch() -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .chrome_executable(CHROMIUM_PATH)
            .disable_default_args()
            .new_headless_mode()
            .no_sandbox()
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg("--no-first-run")
            .arg("--mute-audio");

        // Route external traffic through the agent proxy. Chromium bypasses
        // loopback by default, so local fixture servers are reached directly.
        if let Ok(proxy) = std::env::var("HTTPS_PROXY") {
            if !proxy.is_empty() {
                builder = builder.arg(format!("--proxy-server={proxy}"));
            }
        }

        let config = builder
            .build()
            .map_err(|e| anyhow!("browser config: {e}"))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("launch chromium")?;

        // Keep polling the CDP connection for its whole lifetime. Transient event
        // errors must not stop the loop, or every later command would hang.
        let handler = tokio::spawn(async move {
            while handler.next().await.is_some() {}
        });

        let page = browser.new_page("about:blank").await.context("new page")?;

        Ok(Self {
            browser,
            page,
            _handler: handler,
        })
    }

    /// Navigate to a URL and wait for it to settle.
    pub async fn navigate(&self, url: &str) -> Result<SettleInfo> {
        self.page.goto(url).await.context("goto")?;
        let settle = self.wait_settle(Some(10_000)).await?;
        Ok(settle)
    }

    /// Click an element by CSS selector.
    pub async fn click(&self, selector: &str) -> Result<()> {
        let el = self
            .page
            .find_element(selector)
            .await
            .with_context(|| format!("find element to click: {selector}"))?;
        el.click().await.with_context(|| format!("click: {selector}"))?;
        Ok(())
    }

    /// Type a value into an input by CSS selector. Clears existing content first.
    pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        let el = self
            .page
            .find_element(selector)
            .await
            .with_context(|| format!("find element to fill: {selector}"))?;
        el.click().await.ok();
        el.type_str(value)
            .await
            .with_context(|| format!("fill: {selector}"))?;
        Ok(())
    }

    /// Press a key / key-chord (e.g. "Enter") on the currently focused element,
    /// falling back to <body> if nothing is focused.
    pub async fn key_press(&self, keys: &str) -> Result<()> {
        let el = match self.page.find_element(":focus").await {
            Ok(el) => el,
            Err(_) => self
                .page
                .find_element("body")
                .await
                .context("no focus/body element for key_press")?,
        };
        el.press_key(keys)
            .await
            .with_context(|| format!("key_press: {keys}"))?;
        Ok(())
    }

    /// Read the visible text of an element.
    pub async fn get_text(&self, selector: &str) -> Result<String> {
        let el = self
            .page
            .find_element(selector)
            .await
            .with_context(|| format!("find element for text: {selector}"))?;
        let text = el.inner_text().await?.unwrap_or_default();
        Ok(text)
    }

    /// Read an attribute of an element. Returns None if the attribute is absent.
    pub async fn get_attr(&self, selector: &str, attr: &str) -> Result<Option<String>> {
        let el = self
            .page
            .find_element(selector)
            .await
            .with_context(|| format!("find element for attr: {selector}"))?;
        let value = el.attribute(attr).await?;
        Ok(value)
    }

    /// Scroll the window by (dx, dy) pixels.
    pub async fn scroll(&self, dx: i64, dy: i64) -> Result<()> {
        self.page
            .evaluate(format!("window.scrollBy({dx},{dy})"))
            .await
            .context("scroll")?;
        Ok(())
    }

    /// Current page URL.
    pub async fn get_url(&self) -> Result<String> {
        Ok(self.page.url().await?.unwrap_or_default())
    }

    /// Capture a PNG screenshot, returning the raw bytes.
    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(full_page)
            .build();
        let bytes = self.page.screenshot(params).await.context("screenshot")?;
        Ok(bytes)
    }

    /// Return the full serialized DOM (outerHTML of <html>). Used by `observe`
    /// to compute the state signature and extract deterministic facts.
    pub async fn dom_html(&self) -> Result<String> {
        let html: String = self
            .page
            .evaluate("document.documentElement.outerHTML")
            .await?
            .into_value()
            .context("read outerHTML")?;
        Ok(html)
    }

    /// Wait for the page to settle: load event plus a bounded network-idle window.
    ///
    /// We poll readyState for the load signal, then watch for a quiet period with
    /// no new DOM mutations as a cheap proxy for network/render idle. No pixel
    /// diffing — a spinner would defeat that.
    pub async fn wait_settle(&self, timeout_ms: Option<u64>) -> Result<SettleInfo> {
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(10_000));
        let start = Instant::now();

        // Phase 1: wait for document readyState == "complete".
        let mut load = false;
        while start.elapsed() < timeout {
            let state: String = self
                .page
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.into_value().ok())
                .unwrap_or_default();
            if state == "complete" {
                load = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Phase 2: quiescence — DOM node count stable across a short window.
        let mut network_idle = false;
        let mut last_count: i64 = -1;
        let mut stable_since: Option<Instant> = None;
        while start.elapsed() < timeout {
            let count: i64 = self
                .page
                .evaluate("document.getElementsByTagName('*').length")
                .await
                .ok()
                .and_then(|v| v.into_value().ok())
                .unwrap_or(-1);
            if count == last_count && count >= 0 {
                let since = stable_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(400) {
                    network_idle = true;
                    break;
                }
            } else {
                stable_since = None;
                last_count = count;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(SettleInfo {
            load,
            network_idle,
            timed_out: start.elapsed() >= timeout && !(load && network_idle),
        })
    }

    /// Begin a live screencast: CDP streams JPEG frames as the page renders.
    /// Each frame is base64-decoded and written as `frame_NNNNN.jpg` into
    /// `out_dir`. Returns a handle; call `.stop()` to end and get the count.
    ///
    /// This is the "see it live" mechanism for a headless browser — the engine
    /// taps the same CDP connection it drives with. In production these frames
    /// are forwarded to the parent (as protocol notifications) rather than disk.
    pub async fn start_screencast(&self, out_dir: PathBuf, max_width: i64) -> Result<ScreencastHandle> {
        std::fs::create_dir_all(&out_dir).ok();
        let page = self.page.clone();
        let mut frames = page.event_listener::<EventScreencastFrame>().await?;

        page.execute(
            StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(60)
                .max_width(max_width)
                .every_nth_frame(1)
                .build(),
        )
        .await
        .context("start screencast")?;

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let mut n: usize = 0;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    maybe = frames.next() => {
                        let Some(frame) = maybe else { break };
                        let b64: &str = frame.data.as_ref();
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                            let path = out_dir.join(format!("frame_{n:05}.jpg"));
                            let _ = std::fs::write(path, bytes);
                            n += 1;
                        }
                        // Must ack or Chromium stops sending frames.
                        let _ = page
                            .execute(ScreencastFrameAckParams::new(frame.session_id))
                            .await;
                    }
                }
            }
            n
        });

        Ok(ScreencastHandle {
            page: self.page.clone(),
            stop: Some(stop_tx),
            task,
        })
    }

    /// Capture a screenshot every `every_ms` into `out_dir` as `frame_NNNNN.png`.
    ///
    /// Fixed-rate fallback for "live view" in headless mode, where CDP screencast
    /// emits frames only on compositor changes (sparse). Heavier than screencast
    /// but yields a smooth, deterministic frame sequence for a video.
    pub async fn start_interval_capture(
        &self,
        out_dir: PathBuf,
        every_ms: u64,
    ) -> Result<ScreencastHandle> {
        std::fs::create_dir_all(&out_dir).ok();
        let page = self.page.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let mut n: usize = 0;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep(Duration::from_millis(every_ms)) => {
                        let params = ScreenshotParams::builder()
                            .format(CaptureScreenshotFormat::Png)
                            .build();
                        if let Ok(bytes) = page.screenshot(params).await {
                            let path = out_dir.join(format!("frame_{n:05}.png"));
                            let _ = std::fs::write(path, bytes);
                            n += 1;
                        }
                    }
                }
            }
            n
        });
        Ok(ScreencastHandle {
            page: self.page.clone(),
            stop: Some(stop_tx),
            task,
        })
    }

    /// Close the browser cleanly.
    pub async fn close(mut self) -> Result<()> {
        self.browser.close().await.ok();
        let _ = self.browser.wait().await;
        Ok(())
    }
}
