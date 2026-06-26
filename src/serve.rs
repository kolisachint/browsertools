//! `serve` mode: the native, binary-first interface.
//!
//! A long-running process that reads newline-delimited JSON requests on stdin
//! and writes responses on stdout, with the browser persisting in-process across
//! calls. The shape is deliberately JSON-RPC-flavoured (`{id, method, params}` →
//! `{id, result|error}`) so the later MCP adapter is a trivial reskin.
//!
//! This dispatch loop is also where live view will hang off: an `action` event
//! emitted before each primitive executes, synced to the parent's tool calls.

use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::driver::Driver;

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

        if req.method == "shutdown" {
            write_line(&mut stdout, &json!({"id": req.id, "result": {"ok": true}})).await?;
            break;
        }

        let resp = match dispatch(&driver, &req.method, &req.params).await {
            Ok(result) => Response { id: req.id, result: Some(result), error: None },
            Err(e) => Response {
                id: req.id,
                result: None,
                error: Some(RpcError { message: format!("{e:#}") }),
            },
        };
        write_line(&mut stdout, &serde_json::to_value(&resp)?).await?;
    }

    driver.close().await.ok();
    Ok(())
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
            d.fill(&str_param(p, "selector")?, &str_param(p, "value")?).await?;
            json!({"ok": true})
        }
        "select" => {
            d.select(&str_param(p, "selector")?, &str_param(p, "value")?).await?;
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
            let v = d.get_attr(&str_param(p, "selector")?, &str_param(p, "attr")?).await?;
            json!({ "value": v })
        }
        "get_url" => json!({"url": d.get_url().await?}),
        "screenshot" => {
            let full = p.get("full_page").and_then(|v| v.as_bool()).unwrap_or(false);
            let png = d.screenshot(full).await?;
            let hash = blake3::hash(&png).to_hex().to_string();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
            json!({"hash": hash, "len": png.len(), "png_base64": b64})
        }
        "observe" => serde_json::to_value(d.observe().await?)?,
        other => return Err(anyhow::anyhow!("unknown method: {other}")),
    })
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> Result<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
