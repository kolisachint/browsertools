//! Live view: watch the headless browser act in real time, synced to the
//! parent's tool calls.
//!
//! Primary transport (this module): CDP-driven JPEG frames + per-tool-call
//! `action` events streamed over a WebSocket to a tiny viewer page the human
//! opens. The viewer shows the current page and a log of `▶ click ...` events as
//! the agent issues them. Fallback transport (DevTools remote-debugging) is
//! handled in `serve` via `Driver::devtools_endpoint`.

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use tokio::sync::broadcast;

use crate::driver::{Driver, ScreencastHandle};

/// A message pushed to connected viewers.
#[derive(Clone)]
enum LiveMsg {
    Frame(String),  // base64 JPEG
    Action(String), // e.g. "▶ click .product_pod h3 a"
}

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<LiveMsg>,
}

/// A running live-view session. Drop or `stop()` to tear it down.
pub struct LiveView {
    tx: broadcast::Sender<LiveMsg>,
    url: String,
    _frames: ScreencastHandle,
    server: tokio::task::JoinHandle<()>,
    frame_pump: tokio::task::JoinHandle<()>,
}

impl LiveView {
    /// Start a WebSocket live view for the given driver at ~6 fps.
    pub async fn start(driver: &Driver) -> Result<LiveView> {
        let (tx, _rx) = broadcast::channel::<LiveMsg>(64);

        // Frame source: interval JPEG capture into a raw base64 channel, then
        // re-published as LiveMsg::Frame so frames and actions share one stream.
        //
        // Headless: a brisk 160ms (~6fps) since the only way to "see" the page is
        // this stream. Headful: the user already watches the real window, and each
        // CDP screenshot forces a synchronous compositor frame that visibly fights
        // that window (flicker), so capture far less often — just enough to keep
        // the mirror roughly current.
        let interval_ms = if driver.is_headful() { 500 } else { 160 };
        let (raw_tx, mut raw_rx) = broadcast::channel::<String>(8);
        let frames = driver.start_frame_stream(raw_tx, interval_ms).await?;
        let pump_tx = tx.clone();
        let frame_pump = tokio::spawn(async move {
            while let Ok(b64) = raw_rx.recv().await {
                let _ = pump_tx.send(LiveMsg::Frame(b64));
            }
        });

        // Bind the viewer server on a random loopback port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let url = format!("http://127.0.0.1:{port}/");

        let state = AppState { tx: tx.clone() };
        let app = Router::new()
            .route("/", get(index))
            .route("/ws", get(ws_handler))
            .with_state(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Ok(LiveView {
            tx,
            url,
            _frames: frames,
            server,
            frame_pump,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Emit an action event, shown live in the viewer before the call executes.
    pub fn action(&self, text: impl Into<String>) {
        let _ = self.tx.send(LiveMsg::Action(text.into()));
    }

    pub async fn stop(self) {
        let _ = self._frames.stop().await;
        self.frame_pump.abort();
        self.server.abort();
    }
}

async fn index() -> impl IntoResponse {
    Html(VIEWER_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client(socket, state.tx.subscribe()))
}

async fn client(mut socket: WebSocket, mut rx: broadcast::Receiver<LiveMsg>) {
    loop {
        match rx.recv().await {
            Ok(LiveMsg::Frame(b64)) => {
                let msg = json!({"type": "frame", "data": b64}).to_string();
                if socket.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            Ok(LiveMsg::Action(text)) => {
                let msg = json!({"type": "action", "text": text}).to_string();
                if socket.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

const VIEWER_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>browsertools — live</title>
<style>
  body { margin:0; background:#111; color:#ddd; font-family:system-ui,sans-serif; display:flex; height:100vh; }
  #stage { flex:1; display:flex; align-items:center; justify-content:center; overflow:auto; }
  #screen { max-width:100%; max-height:100vh; box-shadow:0 0 30px #000; background:#fff; }
  #side { width:320px; border-left:1px solid #333; display:flex; flex-direction:column; }
  #side h2 { margin:0; padding:.7rem 1rem; font-size:.8rem; letter-spacing:.08em; text-transform:uppercase; color:#9cd; border-bottom:1px solid #333; }
  #log { flex:1; overflow:auto; margin:0; padding:.5rem; font:13px/1.5 ui-monospace,monospace; }
  #log li { list-style:none; padding:.25rem .4rem; border-bottom:1px solid #1c1c1c; color:#bdf; }
  #status { padding:.5rem 1rem; font-size:.75rem; color:#888; border-top:1px solid #333; }
</style></head>
<body>
  <div id="stage"><img id="screen" alt="waiting for first frame…"></div>
  <div id="side">
    <h2>Agent tool calls</h2>
    <ul id="log"></ul>
    <div id="status">connecting…</div>
  </div>
<script>
  const img = document.getElementById('screen');
  const log = document.getElementById('log');
  const status = document.getElementById('status');
  const ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => status.textContent = 'live';
  ws.onclose = () => status.textContent = 'disconnected';
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.type === 'frame') {
      img.src = 'data:image/jpeg;base64,' + m.data;
    } else if (m.type === 'action') {
      const li = document.createElement('li');
      li.textContent = m.text;
      log.appendChild(li);
      log.scrollTop = log.scrollHeight;
    }
  };
</script>
</body></html>"#;
