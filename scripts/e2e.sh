#!/usr/bin/env bash
# End-to-end smoke: exercises every browser-driving surface against the local
# fixture site with real Chromium. These paths are #[ignore]'d in `cargo test`
# (they need Chromium + this environment's proxy/NSS trust), so plain CI never
# runs them — this script is the gate that does.
#
# Surfaces covered:
#   1. replay thesis      — run-flow CLI, 20x deterministic replay
#   2. serve primitives   — stdio JSON-RPC protocol
#   3. tier 2             — yield NeedsParent (reidentify + decide), resume
#   4. live view          — WebSocket screencast + synced action events (opt)
#
# Usage:  bash scripts/e2e.sh
# Exit:   0 only if every selected surface passes.
#
# The live-view check needs python `websockets`; if it is missing the step is
# skipped (not failed) unless E2E_STRICT=1 is set.
set -uo pipefail

cd "$(dirname "$0")/.."

PASS=0
FAIL=0
note() { printf '\n=== %s ===\n' "$1"; }
ok()   { printf 'PASS: %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf 'FAIL: %s\n' "$1"; FAIL=$((FAIL + 1)); }

note "build"
cargo build --quiet || { echo "build failed"; exit 1; }
BIN="target/debug/browsertools"

# 1. Replay thesis (self-hosts its fixture server on :8734).
note "replay thesis (20x)"
if cargo test --test thesis_replay -- --ignored --nocapture; then
  ok "replay_thesis"
else
  bad "replay_thesis"
fi

# 2. Serve primitives (self-hosts its fixture server on :8732).
note "serve primitives"
if cargo test --test serve_primitives -- --ignored --nocapture; then
  ok "serve_primitives"
else
  bad "serve_primitives"
fi

# 3. Tier 2 parent-in-the-loop (each self-hosts its fixture server).
note "tier 2 reidentify (yield/resume)"
if cargo test --test tier2_reidentify -- --ignored --nocapture; then
  ok "tier2_reidentify"
else
  bad "tier2_reidentify"
fi

note "tier 2 decide (delegated next action)"
if cargo test --test tier2_decide -- --ignored --nocapture; then
  ok "tier2_decide"
else
  bad "tier2_decide"
fi

note "tier 2 judgment (classify / verify_visual / extract_semantic)"
if cargo test --test tier2_judgment -- --ignored --nocapture; then
  ok "tier2_judgment"
else
  bad "tier2_judgment"
fi

# 4. Live view (needs an external fixture server + python websockets).
note "live view (websocket)"
if python3 -c "import websockets" 2>/dev/null; then
  python3 -m http.server 8732 --directory fixtures/site >/dev/null 2>&1 &
  FIX=$!
  trap '[ -n "${FIX:-}" ] && kill "$FIX" 2>/dev/null' EXIT
  sleep 1
  if BT_BIN="$BIN" python3 scripts/live_view_check.py; then
    ok "live_view"
  else
    bad "live_view"
  fi
  kill "$FIX" 2>/dev/null; FIX=
  trap - EXIT
else
  if [ "${E2E_STRICT:-0}" = "1" ]; then
    bad "live_view (python 'websockets' missing; E2E_STRICT=1)"
  else
    echo "SKIP: live_view (python 'websockets' not installed; pip install websockets)"
  fi
fi

note "summary"
printf 'passed=%d failed=%d\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
