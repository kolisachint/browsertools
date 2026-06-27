//! `serve` mode: the native, binary-first interface.
//!
//! A long-running process that reads newline-delimited JSON requests on stdin
//! and writes responses on stdout, with the browser persisting in-process across
//! calls. The shape is deliberately JSON-RPC-flavoured (`{id, method, params}` →
//! `{id, result|error}`) so the later MCP adapter is a trivial reskin.
//!
//! This dispatch loop is also where live view will hang off: an `action` event
//! emitted before each primitive executes, synced to the parent's tool calls.

use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::contract::ParentResponse;
use crate::driver::Driver;
use crate::flow::Flow;
use crate::liveview::LiveView;
use crate::replay::{FlowRun, Progress};

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcError {
    message: String,
}

#[derive(Serialize)]
struct Response {
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

fn str_param(p: &Value, key: &str) -> Result<String> {
    p.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing string param '{key}'"))
}

/// Run the serve loop until stdin closes or a `shutdown` request arrives.
pub async fn run() -> Result<()> {
    let driver = Driver::launch().await?;

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    // Live view is off by default; the parent turns it on via live_view_start.
    let mut live: Option<LiveView> = None;

    // A suspended Tier-2 flow waiting on the parent, if any.
    let mut flow_run: Option<FlowRun> = None;

    // Announce readiness so the parent knows the browser is up.
    write_line(&mut stdout, &json!({"event": "ready"})).await?;

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                write_line(
                    &mut stdout,
                    &json!({"id": Value::Null, "error": {"message": format!("parse error: {e}")}}),
                )
                .await?;
                continue;
            }
        };

        // Lifecycle methods handled here (need the LiveView/loop scope).
        match req.method.as_str() {
            "shutdown" => {
                write_line(&mut stdout, &json!({"id": req.id, "result": {"ok": true}})).await?;
                break;
            }
            "live_view_start" => {
                let result = match LiveView::start(&driver).await {
                    Ok(lv) => {
                        let url = lv.url().to_string();
                        live = Some(lv);
                        json!({"url": url, "transport": "websocket"})
                    }
                    Err(e) => json!({"error": format!("{e:#}")}),
                };
                // Also surface the DevTools fallback endpoint.
                let devtools = driver.devtools_endpoint();
                let mut result = result;
                result["devtools_ws"] = json!(devtools);
                write_line(&mut stdout, &json!({"id": req.id, "result": result})).await?;
                continue;
            }
            "live_view_stop" => {
                if let Some(lv) = live.take() {
                    lv.stop().await;
                }
                write_line(&mut stdout, &json!({"id": req.id, "result": {"ok": true}})).await?;
                continue;
            }
            "flow_start" => {
                if let Some(lv) = &live {
                    lv.action("▶ flow_start".to_string());
                }
                let out = reply(
                    req.id,
                    start_flow(&driver, &req.params, &mut flow_run).await,
                );
                write_line(&mut stdout, &out).await?;
                continue;
            }
            "flow_resume" => {
                if let Some(lv) = &live {
                    lv.action("▶ flow_resume".to_string());
                }
                let out = reply(
                    req.id,
                    resume_flow(&driver, &req.params, &mut flow_run).await,
                );
                write_line(&mut stdout, &out).await?;
                continue;
            }
            "get_resource" => {
                let out = reply(req.id, get_resource(&flow_run, &req.params));
                write_line(&mut stdout, &out).await?;
                continue;
            }
            _ => {}
        }

        // Emit a live action event before the primitive executes, so the viewer
        // shows the parent's tool call in the moment.
        if let Some(lv) = &live {
            lv.action(describe(&req.method, &req.params));
        }

        let resp = match dispatch(&driver, &req.method, &req.params).await {
            Ok(result) => Response {
                id: req.id,
                result: Some(result),
                error: None,
            },
            Err(e) => Response {
                id: req.id,
                result: None,
                error: Some(RpcError {
                    message: format!("{e:#}"),
                }),
            },
        };
        write_line(&mut stdout, &serde_json::to_value(&resp)?).await?;
    }

    if let Some(lv) = live.take() {
        lv.stop().await;
    }
    driver.close().await.ok();
    Ok(())
}

/// Human-readable one-liner for a tool call, shown in the live viewer.
fn describe(method: &str, p: &Value) -> String {
    let target = p
        .get("selector")
        .or_else(|| p.get("url"))
        .or_else(|| p.get("keys"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(value) = p.get("value").and_then(|v| v.as_str()) {
        format!("▶ {method} {target} = {value:?}")
    } else if target.is_empty() {
        format!("▶ {method}")
    } else {
        format!("▶ {method} {target}")
    }
}

/// Map a method name + params to a driver call, returning the JSON result.
async fn dispatch(d: &Driver, method: &str, p: &Value) -> Result<Value> {
    Ok(match method {
        "navigate" => {
            let settle = d.navigate(&str_param(p, "url")?).await?;
            serde_json::to_value(settle)?
        }
        "click" => {
            d.click(&str_param(p, "selector")?).await?;
            json!({"ok": true})
        }
        "fill" => {
            d.fill(&str_param(p, "selector")?, &str_param(p, "value")?)
                .await?;
            json!({"ok": true})
        }
        "select" => {
            d.select(&str_param(p, "selector")?, &str_param(p, "value")?)
                .await?;
            json!({"ok": true})
        }
        "hover" => {
            d.hover(&str_param(p, "selector")?).await?;
            json!({"ok": true})
        }
        "scroll" => {
            let dx = p.get("dx").and_then(|v| v.as_i64()).unwrap_or(0);
            let dy = p.get("dy").and_then(|v| v.as_i64()).unwrap_or(0);
            d.scroll(dx, dy).await?;
            json!({"ok": true})
        }
        "key_press" => {
            d.key_press(&str_param(p, "keys")?).await?;
            json!({"ok": true})
        }
        "wait_settle" => {
            let timeout = p.get("timeout_ms").and_then(|v| v.as_u64());
            serde_json::to_value(d.wait_settle(timeout).await?)?
        }
        "get_text" => json!({"text": d.get_text(&str_param(p, "selector")?).await?}),
        "get_attr" => {
            let v = d
                .get_attr(&str_param(p, "selector")?, &str_param(p, "attr")?)
                .await?;
            json!({ "value": v })
        }
        "get_url" => json!({"url": d.get_url().await?}),
        "screenshot" => {
            let full = p
                .get("full_page")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let png = d.screenshot(full).await?;
            let hash = blake3::hash(&png).to_hex().to_string();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
            json!({"hash": hash, "len": png.len(), "png_base64": b64})
        }
        "observe" => serde_json::to_value(d.observe().await?)?,
        other => return Err(anyhow::anyhow!("unknown method: {other}")),
    })
}

/// Wrap a handler result into a JSON-RPC response envelope.
fn reply(id: Value, result: Result<Value>) -> Value {
    match result {
        Ok(v) => json!({"id": id, "result": v}),
        Err(e) => json!({"id": id, "error": {"message": format!("{e:#}")}}),
    }
}

/// Start a Tier-2 flow. Accepts either an inline `flow` object or a `flow_path`,
/// plus a `vars` map and an optional `store` root. Runs until the flow completes
/// or yields a `ParentRequest`; on a yield the run is parked in `slot`.
async fn start_flow(d: &Driver, p: &Value, slot: &mut Option<FlowRun>) -> Result<Value> {
    let flow = parse_flow_param(p)?;
    let vars = parse_vars(p);
    let store = p.get("store").and_then(|v| v.as_str()).map(PathBuf::from);
    let mut run = FlowRun::new(flow, vars, store)?;
    let progress = run.advance(d).await?;
    Ok(progress_json(progress, run, slot))
}

/// Resume a parked flow with the parent's typed `response` for `token`.
async fn resume_flow(d: &Driver, p: &Value, slot: &mut Option<FlowRun>) -> Result<Value> {
    let token = str_param(p, "token")?;
    let response: ParentResponse = serde_json::from_value(
        p.get("response")
            .cloned()
            .ok_or_else(|| anyhow!("missing 'response' object"))?,
    )?;
    let mut run = slot
        .take()
        .ok_or_else(|| anyhow!("no suspended flow to resume"))?;
    match run.resume(d, &token, response).await {
        Ok(progress) => Ok(progress_json(progress, run, slot)),
        Err(e) => {
            // Keep the run parked so the parent can retry.
            *slot = Some(run);
            Err(e)
        }
    }
}

/// Serialize a `Progress`, parking the run in `slot` iff it paused.
fn progress_json(progress: Progress, run: FlowRun, slot: &mut Option<FlowRun>) -> Value {
    match progress {
        Progress::Done(result) => {
            *slot = None;
            json!({"outcome": "complete", "result": result})
        }
        Progress::Paused { request, token } => {
            *slot = Some(run);
            json!({"outcome": "needs_parent", "request": request, "token": token})
        }
    }
}

/// Fetch the bytes a yielded `ParentRequest` referenced (its `screenshot_ref`),
/// resolved against the currently suspended flow. Lets the parent actually see
/// what it is being asked to reason over.
fn get_resource(slot: &Option<FlowRun>, p: &Value) -> Result<Value> {
    let id = str_param(p, "ref")?;
    let run = slot
        .as_ref()
        .ok_or_else(|| anyhow!("no suspended flow holds resources"))?;
    let bytes = run
        .resource(&id)
        .ok_or_else(|| anyhow!("unknown resource ref: {id}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({"ref": id, "mime": "image/png", "len": bytes.len(), "png_base64": b64}))
}

fn parse_flow_param(p: &Value) -> Result<Flow> {
    if let Some(obj) = p.get("flow") {
        serde_json::from_value(obj.clone()).map_err(|e| anyhow!("invalid inline flow: {e}"))
    } else if let Some(path) = p.get("flow_path").and_then(|v| v.as_str()) {
        Flow::load(path)
    } else {
        Err(anyhow!(
            "flow_start needs 'flow' (inline object) or 'flow_path'"
        ))
    }
}

fn parse_vars(p: &Value) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Some(obj) = p.get("vars").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                m.insert(k.clone(), s.to_string());
            }
        }
    }
    m
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> Result<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
