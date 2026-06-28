//! Browser driver: a thin, deterministic wrapper over chromiumoxide (raw CDP).
//!
//! This is the "dumb hands" layer. It drives Chromium and returns raw facts
//! (bytes, strings, structured DOM observations). It makes no decisions and no
//! LLM calls. Higher layers (replayer, serve) interpret results.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, StopScreencastParams};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Path to the pre-installed Chromium in this environment.
/// Set CHROME_PATH to override.
fn chromium_path() -> String {
    std::env::var("CHROME_PATH").unwrap_or_else(|_| "/opt/pw-browsers/chromium".to_string())
}

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
    /// Launch a Chromium and open a blank page.
    ///
    /// Headless by default; set `BROWSERTOOLS_HEADFUL=1` to launch a real,
    /// on-screen window (useful for watching a flow run live on a desktop).
    ///
    /// Outbound HTTPS in this environment goes through the agent proxy, and TLS is
    /// re-terminated there, so Chromium must (a) route through the proxy and
    /// (b) trust the proxy CA via the NSS store at `$HOME/.pki/nssdb`.
    pub async fn launch() -> Result<Self> {
        // A fresh per-launch profile dir. chromiumoxide otherwise defaults every
        // launch to a single shared `chromiumoxide-runner` dir, whose SingletonLock
        // collides when a prior Chromium lingers or two instances overlap.
        let user_data_dir = std::env::temp_dir().join(format!(
            "browsertools-profile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        let headful = std::env::var("BROWSERTOOLS_HEADFUL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let mut builder = BrowserConfig::builder()
            .chrome_executable(chromium_path())
            .disable_default_args()
            .user_data_dir(&user_data_dir)
            .no_sandbox();
        if !headful {
            builder = builder.new_headless_mode();
        }
        let mut builder = builder
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg("--no-first-run")
            .arg("--mute-audio")
            .arg("--disable-software-rasterizer")
            .arg("--disable-background-networking")
            .arg("--disable-default-apps")
            .arg("--disable-extensions")
            .arg("--disable-sync")
            .arg("--noerrdialogs");

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

        let (browser, mut handler) = Browser::launch(config).await.context("launch chromium")?;

        // Keep polling the CDP connection for its whole lifetime. Transient event
        // errors must not stop the loop, or every later command would hang.
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });

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
        el.click()
            .await
            .with_context(|| format!("click: {selector}"))?;
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

    /// Hover an element (reveals tooltips / dropdowns).
    pub async fn hover(&self, selector: &str) -> Result<()> {
        let el = self
            .page
            .find_element(selector)
            .await
            .with_context(|| format!("find element to hover: {selector}"))?;
        el.hover()
            .await
            .with_context(|| format!("hover: {selector}"))?;
        Ok(())
    }

    /// Choose an option in a <select> by value, dispatching a change event.
    pub async fn select(&self, selector: &str, value: &str) -> Result<()> {
        let js = format!(
            "(() => {{ const e = document.querySelector({sel}); if (!e) return false; \
             e.value = {val}; e.dispatchEvent(new Event('change', {{bubbles:true}})); return true; }})()",
            sel = serde_json::to_string(selector)?,
            val = serde_json::to_string(value)?,
        );
        let ok: bool = self.page.evaluate(js).await?.into_value().unwrap_or(false);
        if !ok {
            return Err(anyhow!("select: element not found: {selector}"));
        }
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

    /// Whether an element matching the selector currently exists.
    pub async fn exists(&self, selector: &str) -> bool {
        self.page.find_element(selector).await.is_ok()
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

    /// Deterministic observation of the current page: DOM-derived facts plus a
    /// content-invariant `state_signature` and a screenshot reference. No
    /// interpretation — that is the parent LLM's job.
    pub async fn observe(&self) -> Result<crate::observe::Observation> {
        // Capture inputs first (these await), then parse synchronously so the
        // non-Send `scraper::Html` never crosses an await point.
        let url = self.get_url().await?;
        let html = self.dom_html().await?;
        let png = self.screenshot(false).await?;
        let screenshot_ref = blake3::hash(&png).to_hex().to_string();

        let facts = crate::observe::analyze(&html);
        Ok(crate::observe::Observation {
            url,
            title: facts.title,
            inputs: facts.inputs,
            landmarks: facts.landmarks,
            text_blocks: facts.text_blocks,
            has_error_region: facts.has_error_region,
            state_signature: facts.state_signature,
            screenshot_ref,
        })
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

    /// Stream JPEG frames (base64) into a broadcast channel every `every_ms`,
    /// for live view. Interval capture is used rather than CDP screencast because
    /// in headless mode compositor-driven frames are sparse; a steady low rate
    /// guarantees the viewer always shows the current page. Returns a handle.
    pub async fn start_frame_stream(
        &self,
        tx: tokio::sync::broadcast::Sender<String>,
        every_ms: u64,
    ) -> Result<ScreencastHandle> {
        let page = self.page.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let mut n: usize = 0;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep(Duration::from_millis(every_ms)) => {
                        let params = ScreenshotParams::builder()
                            .format(CaptureScreenshotFormat::Jpeg)
                            .quality(50)
                            .build();
                        if let Ok(bytes) = page.screenshot(params).await {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            // Ignore send errors (no subscribers yet).
                            let _ = tx.send(b64);
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

    /// The browser's CDP debug WebSocket endpoint — basis for the DevTools
    /// remote-debugging fallback transport for live view.
    pub fn devtools_endpoint(&self) -> String {
        self.browser.websocket_address().clone()
    }

    /// Close the browser cleanly.
    pub async fn close(mut self) -> Result<()> {
        self.browser.close().await.ok();
        let _ = self.browser.wait().await;
        Ok(())
    }
}
