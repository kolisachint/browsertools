# browsertools — Design (locked)

A deterministic browser-automation binary that a parent coding agent (**hoocode**)
calls to run and replay web workflows. The binary makes **no LLM calls**. Where a
judgment is needed that only an LLM can make, the binary **yields** a typed request
back to the parent, which runs its own LLM and resumes.

---

## 1. Thesis

An LLM driving a browser today is stateless and amnesiac: every session
re-discovers everything, burns tokens exploring, and leaves no proof of what it
did. The fix is to **move every LLM decision from per-run to per-flow**:

- **Discover once** (LLM-in-the-parent, expensive): explore a task, record a trace,
  compile it into a canonical flow.
- **Replay cheap** (deterministic, zero LLM): execute the saved flow, verify with
  DOM invariants, write tamper-evident evidence.

The whole system lives or dies on one bet: **deterministic replay survives on a
live site across runs.** The MVP exists to prove or kill exactly that.

---

## 2. Roles

- **Parent (hoocode):** owns the LLM. Drives discovery, interprets screenshots,
  fulfills yield requests. Not in this repo.
- **This binary:** deterministic engine. Drives Chromium, records traces, compiles
  and replays flows, verifies via DOM, writes evidence. Surfaces typed requests
  when an LLM judgment is required.

**No LLM client, no HTTP client, no vision code lives here.** That is permanent.

---

## 3. Surface hierarchy (primary → addon)

Direct binary invocation is the product. MCP is an optional reskin added later.

1. **Engine core** — plain Rust API returning `Outcome`. The real thing; everything
   else is an adapter over it.
2. **Binary (primary):**
   - `run_flow` — **one-shot**: launch headless Chromium, replay a flow, write
     evidence, exit. Self-contained, no session plumbing. The cheap path.
   - `serve` — **long-running**: newline-delimited JSON requests on stdin,
     responses on stdout; browser persists in-process across calls. How the parent
     drives **primitives** as a stateful sequence.
3. **MCP adapter (addon, post-MVP):** wraps the same engine. The native `serve`
   protocol is deliberately JSON-RPC-shaped, so MCP is a trivial envelope + a
   `tools/list`. We do not build toward MCP; it falls out for free.

---

## 4. Primitive tool surface

Deterministic. The parent's LLM interprets; the binary only acts and reports facts.

| Tool | In | Out |
|---|---|---|
| `navigate` | `{url}` | `{url, settle}` |
| `click` | `{selector}` or `{x,y}` | `{ok}` |
| `fill` | `{selector, value}` | `{ok}` |
| `select` | `{selector, value}` | `{ok}` |
| `scroll` | `{selector?, dx, dy}` | `{ok}` |
| `hover` | `{selector}` | `{ok}` |
| `key_press` | `{keys}` | `{ok}` |
| `wait_settle` | `{timeout_ms?}` | `{load, network_idle, timed_out}` |
| `get_text` | `{selector}` | `{text}` |
| `get_attr` | `{selector, attr}` | `{value?}` |
| `get_url` | `{}` | `{url}` |
| `screenshot` | `{full_page?}` | PNG resource + `{hash}` |
| `observe` | `{}` | `Observation` |

`observe()` is load-bearing: it returns **deterministic facts only** — no
`page_state` guess. That interpretation is the parent LLM's job.

```rust
struct Observation {
    url: String,
    title: String,
    inputs: Vec<InputFact>,     // role, accessible_name, kind, selector_hint — no values
    landmarks: Vec<Landmark>,   // role + name of nav/main/form/region
    text_blocks: Vec<String>,   // headings, alerts, salient text
    has_error_region: bool,
    state_signature: String,    // blake3 over normalized DOM skeleton
    screenshot_ref: ResourceId, // parent fetches the PNG if its LLM needs to look
}
```

---

## 5. The yield contract

The engine runs deterministically until it hits something only an LLM can resolve,
then returns a typed request and pauses. The parent fulfills it and calls `resume`.

```rust
enum Outcome {
    Done   { evidence: EvidenceRef },
    Failed { step_id: String, kind: FailKind, detail: String },
    NeedsParent { request: ParentRequest, token: ResumeToken },
}

enum ParentRequest {            // every "vision call" is exactly one of these
    ClassifyState    { screenshot_ref: ResourceId, observation: Observation },
    VerifyVisual     { screenshot_ref: ResourceId, expected_state: String },
    ExtractSemantic  { screenshot_ref: ResourceId, fields: Vec<String> },
    DecideNextAction { screenshot_ref: ResourceId, observation: Observation, goal: String },
    ReidentifyElement{ screenshot_ref: ResourceId, description: String },
}
// parent fulfills via its LLM, then: resume(token, ParentResponse) -> Outcome
```

On the MVP target (`books.toscrape.com`) no `NeedsParent` ever fires — replay
returns `Done` atomically. The variants are **defined now** so the contract is
frozen before Phase 2 needs them; they are **not exercised** in the MVP.

---

## 6. Flow schema (MVP-minimal)

```rust
struct Flow {
    id: String, name: String, version: u32,
    start_url: String,    // auto-navigated before the first step runs
                          // ({{vars}}-resolved); no leading `navigate` needed
    vars:    Vec<VarSpec>,     // { key, required, example }
    steps:   Vec<Step>,
    outputs: Vec<OutputSpec>,  // { key, source }
}

struct Step { id: String, action: Action, on_fail: OnFail }

enum Action {
    Navigate   { url: String },
    Click      { selector: String, fallbacks: Vec<String> },
    Fill       { selector: String, value_tpl: String },   // "{{query}}"
    Select     { selector: String, value_tpl: String },
    WaitSettle,
    Checkpoint { asserts: Vec<Invariant> },
}

enum Invariant {
    ElementPresent(String),
    TextPresent { sel: Option<String>, substr: String },
    UrlMatches(String),
}

enum Source { Text(String), Attr { sel: String, attr: String }, Url } // OutputSpec.source
enum OnFail { Halt, Skip }
```

Verification is **DOM invariants only** — no pixels, no vision. Output extraction
reads the datum straight from the DOM (exact; never OCR a value that lives in text).

---

## 7. Storage layout

```
.hoocode/
  flows/
    <flow_id>/
      flow.json
      runs/
        <iso-timestamp>/
          evidence.png
          extracted.json     # { fields, screenshot_hash }
          trace.json         # full step-by-step execution log
```

**Evidence integrity:** `extracted.json` carries the blake3 hash of `evidence.png`.
If they disagree, the evidence is invalid. (A tamper-evident append-only log is a
later phase; not MVP.)

Durable cross-session/cross-user persistence (a real backend, search index, success
aggregates, shadow runs) is **explicitly deferred**. MVP store is the local
filesystem with a single flow.

---

## 8. Settle & verification policy

- **Page settle:** CDP load event + bounded network-idle. **No pixel-diff loop.**
  (A spinner animates forever and would defeat pixel-stability; treat "network idle
  but still animating" as its own signal, not "keep waiting.")
- **Checkpoint verification:** DOM invariants (element-present / text-present /
  url-matches). Free, robust to content churn, asserts the thing that matters.
- **Screenshot = evidence, not verification.** Vision (in the parent) is the oracle
  of last resort, used only when the DOM is blocked (e.g. cross-origin iframe) or
  genuinely ambiguous. Not exercised in the MVP.

---

## 9. Language & stack

- **Rust-first.** Browser driving via **`chromiumoxide`** (raw CDP), single process,
  single static binary — fast cold start, no node_modules. The MVP target is
  deliberately low-defense, so `chromiumoxide`'s maturity gap vs. Playwright never
  bites.
- **Node enters later, only when forced** — i.e. hostile sites (login / payment /
  captcha / iframes) in Phase 3, as a thin Playwright driver sidecar. Not in MVP.

**Crates (MVP):** `tokio`, `chromiumoxide`, `serde` + `serde_json`, `blake3`,
`anyhow`, `scraper`, `clap`. No `rmcp`, no HTTP, no LLM, no `image`.

The chromiumoxide launch uses the pre-installed browser at
`/opt/pw-browsers/chromium`.

---

## 10. MVP scope

Two deliverables, both shipped and tested:

- **(A) Primitive surface** — exposed to the parent via `serve` (native stdio JSON
  protocol) and tested with a scripted call sequence against a live site.
- **(B) Seeded-flow replayer** — proves the replay thesis.

The MVP **does not auto-discover.** The first flow is hand-seeded (`books.toscrape`
search → results → detail → extract). Discovery (`record` + `compile`) and the
parent-driven exploration loop are Phase 2. This isolates the actual risk —
*does deterministic replay survive?* — and tests it for the price of one binary.

**Out of MVP scope:** login/credentials/secret broker, payment/OTP handoff,
captcha/anti-bot, self-healing, pixel diff, durable backend, success-rate
aggregates, shadow runs, iframes/shadow DOM, vision, MCP, Node.

### Target site

`books.toscrape.com` — purpose-built to be stable and scrapable, no defenses,
predictable structure. The thesis is tested with zero variables outside the bet.

### Success metric (make-or-break)

> The hand-seeded flow replays **N = 20 times across hours**, **zero LLM calls**,
> correct extraction every run, evidence written each run.

If it holds → thesis alive, build Phase 2. If selectors drift on a site *this*
stable → the core bet is fragile, learned in days for the cost of one binary.

---

## 10b. Live view (decided; built right after `serve`)

The human can watch the browser act in real time, synced to the parent's tool
calls. Mechanism, hung off the `serve` dispatch loop:

- Before each primitive executes, `serve` emits an **action event**
  (`▶ click #buy`) — this is what makes it "watch the LLM's tool calls", not just
  "watch a browser". Free: `serve` is already the chokepoint for every call.
- **Primary transport:** CDP `Page.startScreencast` frames + action events
  streamed over a **WebSocket** to a small viewer page the human opens. Headless,
  real-time, read-only.
- **Fallback transport:** Chromium `--remote-debugging-port` + the DevTools
  inspector frontend. Almost no code; exposes full CDP; used when the WS viewer
  isn't wanted.
- Off by default (overhead + privacy). Parent enables via `live_view_start`,
  which returns a URL. Frames ride a separate WS so JSON-RPC stdout stays clean.
- **Reachability:** only useful where the viewer port is reachable (hoocode local
  → `localhost:PORT`, or a port-forwarded/preview env). Not viewable through a
  chat surface.

Order: `observe` → `serve` → **live view** → replayer → `run_flow` thesis test.

## 11. Build order

1. **Engine core + chromiumoxide driver** — primitives as a Rust API.
   *Engine integration test vs `books.toscrape.com`.*
2. **`observe()` + `state_signature`** — blake3 over normalized DOM skeleton.
   *Signature golden/stability test.*
3. **`serve`** — stdio newline-JSON dispatch over the engine; persistent session.
   *Scripted-sequence primitive test.* → **deliverable A**
4. **Flow schema + seeded flow + replayer + DOM checkpoints.**
5. **Evidence writer** — PNG + blake3 hash + `extracted.json` + `trace.json`.
6. **`run_flow` one-shot.** *Thesis test ×20.* → **deliverable B**
7. **Stubs for Phase 2** — `Outcome::NeedsParent`, `record`/`compile` signatures,
   and the MCP adapter (all defined, not implemented).

---

## 12. Phases after MVP (direction, not committed scope)

- **P2:** parent-driven discovery (parent loops primitives, binary records) +
  the real compiler (action minimization, variable induction, selector synthesis,
  output-rule learning) + state-equivalence refinement + the `NeedsParent` yield
  points wired live. MCP adapter ships here as an addon.
- **P3:** hostile sites → Node + Playwright driver sidecar, secret broker, OTP
  handoff, drift classifier, self-healing, versioning/rollback.
- **Long-term:** durable shared store, full-text flow search, shadow-run
  verification keeping `success_rate` / `last_verified` honest, tamper-evident
  evidence log.
