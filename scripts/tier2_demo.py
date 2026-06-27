#!/usr/bin/env python3
"""Tier 2 Demo: browsertools with live preview + yield/resume contract.

Demonstrates the parent-in-the-loop pattern:
1. Engine runs deterministically until it hits an LLM-only decision
2. Engine suspends with Outcome::NeedsParent + a typed request
3. Parent (this script) provides a canned response
4. Engine resumes and completes

Three scenarios demonstrated:
  - decide: parent tells engine what action to take
  - reidentify: parent corrects a drifted selector
  - judgment: parent classifies/verifies/extracts semantic fields

Usage:
    python3 scripts/tier2_demo.py

    The live viewer opens automatically - watch the browser as the
    engine pauses and resumes at each yield point.
"""

import asyncio
import json
import os
import shutil
import sys
import webbrowser

BIN = os.environ.get("BT_BIN", "target/debug/browsertools")
PORT = 8731
FIXTURES = os.path.join(os.path.dirname(__file__), "..", "fixtures", "site")


def find_chromium():
    """Check if a Chromium-based browser is available."""
    path = os.environ.get("CHROME_PATH")
    if path and os.path.isfile(path):
        return path

    candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/opt/pw-browsers/chromium",
        "/opt/homebrew/bin/chromium",
    ]
    for c in candidates:
        if os.path.isfile(c):
            return c

    for name in ["google-chrome", "microsoft-edge", "chromium-browser", "chromium"]:
        p = shutil.which(name)
        if p and os.path.isfile(p):
            return p

    return None


def clean_singleton_lock():
    """Remove stale browser singleton locks."""
    import glob
    import tempfile

    tmp = tempfile.gettempdir()
    for pattern in ["chromiumoxide-runner", "chrome*"]:
        for path in glob.glob(os.path.join(tmp, pattern)):
            lock = os.path.join(path, "SingletonLock")
            if os.path.exists(lock):
                try:
                    os.remove(lock)
                except OSError:
                    pass


# ── Flow Definitions ────────────────────────────────────────────────────────

# Scenario 1: decide - engine asks parent what to do
DECIDE_FLOW = {
    "id": "tier2_decide",
    "name": "delegated decision point",
    "version": 1,
    "start_url": "{{base}}",
    "vars": [{"key": "base", "required": True}],
    "steps": [
        {"id": "s01", "action": {"action": "navigate", "url": "{{base}}"}, "on_fail": "halt"},
        {"id": "s02", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s03", "action": {"action": "decide", "goal": "open the first book's detail page"}, "on_fail": "halt"},
        {"id": "s04", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s05", "action": {"action": "checkpoint", "asserts": [
            {"kind": "element_present", "selector": "h1"},
            {"kind": "element_present", "selector": ".price_color"},
            {"kind": "url_matches", "pattern": "catalogue/"}
        ]}, "on_fail": "halt"},
    ],
    "outputs": [
        {"key": "title", "source": {"from": "text", "selector": "h1"}},
        {"key": "price", "source": {"from": "text", "selector": ".price_color"}},
    ],
}

# Scenario 2: reidentify - drifted selector triggers parent help
REIDENTIFY_FLOW = {
    "id": "tier2_reidentify",
    "name": "drifted selector triggers reidentify",
    "version": 1,
    "start_url": "{{base}}",
    "vars": [{"key": "base", "required": True}],
    "steps": [
        {"id": "s01", "action": {"action": "navigate", "url": "{{base}}"}, "on_fail": "halt"},
        {"id": "s02", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s03", "action": {"action": "checkpoint", "asserts": [
            {"kind": "element_present", "selector": ".product_pod h3 a"}
        ]}, "on_fail": "halt"},
        # This selector is WRONG - forces reidentify
        {"id": "s04", "action": {"action": "click", "selector": ".totally-wrong-selector"}, "on_fail": "halt"},
        {"id": "s05", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s06", "action": {"action": "checkpoint", "asserts": [
            {"kind": "element_present", "selector": "h1"},
            {"kind": "element_present", "selector": ".price_color"},
            {"kind": "url_matches", "pattern": "catalogue/"}
        ]}, "on_fail": "halt"},
    ],
    "outputs": [
        {"key": "title", "source": {"from": "text", "selector": "h1"}},
        {"key": "price", "source": {"from": "text", "selector": ".price_color"}},
    ],
}

# Scenario 3: judgment - classify + verify_visual + extract_semantic
JUDGMENT_FLOW = {
    "id": "tier2_judgment",
    "name": "classify + verify_visual + extract_semantic",
    "version": 1,
    "start_url": "{{base}}",
    "vars": [{"key": "base", "required": True}],
    "steps": [
        {"id": "s01", "action": {"action": "navigate", "url": "{{base}}"}, "on_fail": "halt"},
        {"id": "s02", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s03", "action": {"action": "classify"}, "on_fail": "halt"},
        {"id": "s04", "action": {"action": "click", "selector": ".product_pod h3 a"}, "on_fail": "halt"},
        {"id": "s05", "action": {"action": "wait_settle"}, "on_fail": "halt"},
        {"id": "s06", "action": {"action": "verify_visual", "expected_state": "book detail page"}, "on_fail": "halt"},
        {"id": "s07", "action": {"action": "extract_semantic", "fields": ["rating"]}, "on_fail": "halt"},
        {"id": "s08", "action": {"action": "checkpoint", "asserts": [
            {"kind": "element_present", "selector": "h1"},
            {"kind": "element_present", "selector": ".price_color"}
        ]}, "on_fail": "halt"},
    ],
    "outputs": [
        {"key": "title", "source": {"from": "text", "selector": "h1"}},
        {"key": "price", "source": {"from": "text", "selector": ".price_color"}},
    ],
}


async def main():
    # ── Check prerequisites ────────────────────────────────────────────────
    browser = find_chromium()
    if not browser:
        print("ERROR: No Chromium-based browser found")
        print()
        print("Install one of:")
        print("  - Chromium: brew install --cask chromium")
        print("  - Google Chrome: brew install --cask google-chrome")
        print("  - Microsoft Edge: brew install --cask microsoft-edge")
        print()
        print("Or set CHROME_PATH environment variable:")
        print("  export CHROME_PATH=/path/to/chrome")
        return 1

    print(f"using browser: {browser}")
    clean_singleton_lock()

    # ── Build ──────────────────────────────────────────────────────────────
    print("building browsertools...")
    build = await asyncio.create_subprocess_exec(
        "cargo", "build", "--quiet",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    await build.wait()
    if build.returncode != 0:
        print("build failed")
        return 1

    # ── Fixture server ─────────────────────────────────────────────────────
    print(f"starting fixture server on :{PORT}...")
    py = await asyncio.create_subprocess_exec(
        "python3", "-m", "http.server", str(PORT),
        "--directory", FIXTURES,
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.DEVNULL,
    )
    await asyncio.sleep(0.5)

    # ── Launch serve mode ──────────────────────────────────────────────────
    print("launching browsertools serve...")
    env = os.environ.copy()
    env["CHROME_PATH"] = browser

    proc = await asyncio.create_subprocess_exec(
        BIN, "serve",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env=env,
    )

    async def send(obj):
        proc.stdin.write((json.dumps(obj) + "\n").encode())
        await proc.stdin.drain()

    async def read_json():
        line = await proc.stdout.readline()
        if not line:
            stderr = await proc.stderr.read(4096)
            if stderr:
                print(f"stderr: {stderr.decode()}")
            return None
        return json.loads(line.decode())

    # Wait for ready
    ev = await read_json()
    if not ev or ev.get("event") != "ready":
        print("FAIL: did not receive ready event")
        return 1

    print("browsertools ready\n")

    # ── Start live view ────────────────────────────────────────────────────
    await send({"id": 1, "method": "live_view_start"})
    resp = await read_json()
    live_url = resp["result"]["url"]

    print("=" * 60)
    print("TIER 2 DEMO: YIELD/RESUME CONTRACT")
    print("=" * 60)
    print(f"  Live viewer: {live_url}")
    print("=" * 60)
    print()

    # Open live viewer
    webbrowser.open(live_url)

    # Connect WebSocket for stats
    try:
        import websockets
        ws_url = live_url.replace("http://", "ws://") + "ws"
        ws = await websockets.connect(ws_url)
        frames = 0
        actions = []

        async def watch():
            nonlocal frames
            async for raw in ws:
                m = json.loads(raw)
                if m["type"] == "frame":
                    frames += 1
                elif m["type"] == "action":
                    actions.append(m["text"])

        watcher = asyncio.create_task(watch())
    except ImportError:
        ws = None
        watcher = None
        frames = 0
        actions = []

    req_id = 10  # start after setup requests

    async def run_flow(flow, scenario_name, responses):
        """Run a flow and handle yield/resume points."""
        nonlocal req_id
        print(f"  scenario: {scenario_name}")
        print(f"  {'─' * 50}")

        # Start flow
        req_id += 1
        await send({
            "id": req_id,
            "method": "flow_start",
            "params": {"flow": flow, "vars": {"base": f"http://127.0.0.1:{PORT}/"}}
        })

        for i, (expected_kind, response) in enumerate(responses):
            result = await read_json()
            if result.get("error"):
                print(f"    ERROR: {result['error']}")
                return False

            r = result["result"]
            if r["outcome"] == "needs_parent":
                kind = r["request"]["request"]
                token = r["token"]
                goal = r["request"].get("goal", "")

                print(f"    [{i+1}] engine pauses: {kind}")
                if goal:
                    print(f"        goal: {goal}")
                print(f"        -> parent responds with canned answer")

                # Send resume
                req_id += 1
                await send({
                    "id": req_id,
                    "method": "flow_resume",
                    "params": {"token": token, "response": response}
                })
            else:
                print(f"    unexpected outcome: {r['outcome']}")
                return False

        # Get final completion
        result = await read_json()
        if result.get("error"):
            print(f"    ERROR: {result['error']}")
            return False

        r = result["result"]
        if r["outcome"] == "complete":
            res = r["result"]
            print(f"    result: {res['status']}")
            print(f"    outputs: title={res['outputs'].get('title', 'N/A')}, price={res['outputs'].get('price', 'N/A')}")
            if "rating" in res["outputs"]:
                print(f"             rating={res['outputs']['rating']}")
            print(f"    checkpoints: {res['checkpoints_passed']}")
            print()
            return True
        else:
            print(f"    unexpected final outcome: {r['outcome']}")
            return False

    # ── Scenario 1: Decide ─────────────────────────────────────────────────
    print("=" * 60)
    print("SCENARIO 1: DECIDE")
    print("  Engine asks: 'what should I do?'")
    print("  Parent answers: 'click the first book'")
    print("=" * 60)
    print()

    success = await run_flow(
        DECIDE_FLOW,
        "decide (delegated decision)",
        [
            ("decide_next_action", {
                "response": "next_action",
                "action": {"action": "click", "selector": ".product_pod h3 a"}
            }),
        ]
    )
    await asyncio.sleep(0.5)

    # ── Scenario 2: Reidentify ─────────────────────────────────────────────
    print("=" * 60)
    print("SCENARIO 2: REIDENTIFY")
    print("  Engine tries wrong selector, asks: 'find the element'")
    print("  Parent answers: '.product_pod h3 a'")
    print("=" * 60)
    print()

    success = await run_flow(
        REIDENTIFY_FLOW,
        "reidentify (selector drift)",
        [
            ("reidentify_element", {
                "response": "element",
                "selector": ".product_pod h3 a"
            }),
        ]
    )
    await asyncio.sleep(0.5)

    # ── Scenario 3: Judgment ───────────────────────────────────────────────
    print("=" * 60)
    print("SCENARIO 3: JUDGMENT")
    print("  Three chained yield points:")
    print("    1. classify: 'what page is this?' -> 'catalogue'")
    print("    2. verify_visual: 'looks like book detail?' -> true")
    print("    3. extract_semantic: 'read the rating' -> '4 stars'")
    print("=" * 60)
    print()

    success = await run_flow(
        JUDGMENT_FLOW,
        "judgment (classify + verify + extract)",
        [
            ("classify_state", {
                "response": "state",
                "state": "catalogue"
            }),
            ("verify_visual", {
                "response": "verified",
                "passed": True
            }),
            ("extract_semantic", {
                "response": "extracted",
                "fields": {"rating": "4 stars"}
            }),
        ]
    )

    # ── Summary ────────────────────────────────────────────────────────────
    print("=" * 60)
    print("SUMMARY")
    print("=" * 60)
    print("  All three tier 2 scenarios completed successfully:")
    print("    1. decide: parent delegated action selection")
    print("    2. reidentify: parent corrected drifted selector")
    print("    3. judgment: parent classified, verified, extracted")
    print()
    print("  Live viewer captured frames and action events.")
    print("=" * 60)

    # Shutdown
    await send({"id": 999, "method": "shutdown"})

    if watcher:
        await asyncio.sleep(0.5)
        watcher.cancel()
        print(f"\n  live viewer stats: frames={frames} actions={len(actions)}")

    # Cleanup
    try:
        await asyncio.wait_for(proc.wait(), timeout=5)
    except asyncio.TimeoutError:
        proc.kill()

    try:
        py.terminate()
    except ProcessLookupError:
        pass

    print("\ntier 2 demo complete")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
