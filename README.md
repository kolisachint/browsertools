# browsertools

A deterministic browser engine with **no LLM in-process**. It drives Chromium
over raw CDP (via `chromiumoxide`) and exposes three things:

- **`run-flow`** — replay a saved flow once, deterministically, and write a
  tamper-evident evidence bundle. Zero LLM calls.
- **`serve`** — a long-running stdio JSON-RPC server exposing the browser
  primitives (`navigate`, `click`, `fill`, `observe`, …) to a parent process.
- **live view** — a WebSocket screencast of the live page with synced action
  events, for watching a session in real time.

The thesis: once a flow is recorded, replaying it is fully deterministic and
needs no model. The parent (an LLM agent) is only involved when *discovering* a
flow or resolving genuine ambiguity — never on the hot replay path. See
`DESIGN.md` for the full architecture and the Phase 2/3 escalation contract
(`src/contract.rs`).

## Build

Chromium is expected at `/opt/pw-browsers/chromium` (this environment ships it).

```bash
cargo build
```

## Replay a flow

A flow is JSON: a list of steps (navigate / click / fill / select / wait /
checkpoint) plus output extraction rules and `{{var}}` placeholders. See
`fixtures/flows/bookstore_first_book.json`.

```bash
# Serve the bundled fixture site, then replay against it.
python3 -m http.server 8734 --directory fixtures/site &
target/debug/browsertools run-flow \
  --flow fixtures/flows/bookstore_first_book.json \
  --var base=http://127.0.0.1:8734/ \
  --store .hoocode
```

`run-flow` prints the `RunResult` JSON (status, per-step trace, extracted
outputs, screenshot hash) and exits non-zero if the flow failed. The evidence
bundle is written under:

```
.hoocode/flows/<flow_id>/runs/<run_id>/
  evidence.png      full-page screenshot
  extracted.json    extracted fields + screenshot hash
  trace.json        the full RunResult
```

## Serve mode (primitives over stdio)

Newline-delimited JSON requests in, responses out. On start it emits
`{"event":"ready"}`.

```bash
target/debug/browsertools serve
# then write requests to stdin, one JSON object per line:
{"id":1,"method":"navigate","params":{"url":"http://127.0.0.1:8732/"}}
{"id":2,"method":"observe"}
{"id":3,"method":"shutdown"}
```

## Parent-in-the-loop replay (Tier 2)

Replay normally needs no model. When a flow hits something only an LLM can
resolve — today, a click whose selector has *drifted* off the live DOM — the
engine does not fail: it **suspends** and yields a typed `ParentRequest` so the
parent (the LLM agent) can resolve it, then **resumes** deterministically. This
runs over `serve`, where the browser persists across the suspension.

```jsonc
// Start a flow (inline `flow` object or `flow_path`, plus vars):
{"id":1,"method":"flow_start","params":{"flow":{...},"vars":{"base":"http://…/"}}}

// If the engine needs help it replies with a pause instead of a result:
{"id":1,"result":{"outcome":"needs_parent",
  "request":{"request":"reidentify_element","screenshot_ref":"…","description":"…"},
  "token":"run_…:1"}}

// The parent resolves it and resumes with a typed response:
{"id":2,"method":"flow_resume","params":{"token":"run_…:1",
  "response":{"response":"element","selector":".product_pod h3 a"}}}

// …which runs to completion (or pauses again):
{"id":2,"result":{"outcome":"complete","result":{"status":"success", …}}}
```

The request/response variants are the frozen contract in `src/contract.rs`;
`ReidentifyElement` is the one wired today, the rest drop into the same
pause/resume machinery. See `tests/tier2_reidentify.rs` for a full round-trip.

## Live view

`serve` exposes `live_view_start`, which returns an HTTP URL; connect a
WebSocket to `<url>ws` to receive JPEG `frame` messages and `action` events.
`scripts/live_view_check.py` is a working client.

## Tests

Fast, pure-logic tests run with plain `cargo test`. The browser-driving paths
need real Chromium and are marked `#[ignore]`:

```bash
# The whole end-to-end gate (replay thesis + serve + tier 2 + live view):
bash scripts/e2e.sh

# Or individually:
cargo test --test thesis_replay     -- --ignored --nocapture   # 20x deterministic replay
cargo test --test serve_primitives  -- --ignored --nocapture   # stdio protocol
cargo test --test tier2_reidentify  -- --ignored --nocapture   # yield/resume contract
```

The thesis test asserts extraction is identical across 20 cold runs. It also
reports whether the rendered screenshot was byte-identical; set
`THESIS_STRICT_PIXELS=1` to make pixel-identity a hard requirement.
