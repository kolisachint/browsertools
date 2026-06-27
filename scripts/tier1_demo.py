#!/usr/bin/env python3
"""Tier 1 Demo: browsertools with live preview.

Starts the fixture site, launches browsertools serve mode with live view,
drives a simple action sequence, and prints the live viewer URL.

Prerequisites:
    - Chromium-based browser (Chrome, Edge, or Chromium)
    - cargo build completed
    - python3 with websockets (optional, for frame stats)

Usage:
    python3 scripts/tier1_demo.py

    Then open the printed URL in your browser to watch the live preview.
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
    # Check environment variable first
    path = os.environ.get("CHROME_PATH")
    if path and os.path.isfile(path):
        return path

    # Direct binary paths (most reliable)
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

    # PATH lookup
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

    # Clean up stale locks from previous runs
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
            # Check for error on stderr
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
    devtools = resp["result"].get("devtools_ws", "")

    print("=" * 60)
    print("LIVE VIEWER")
    print("=" * 60)
    print(f"  Open in browser: {live_url}")
    print(f"  DevTools fallback: {devtools[:48]}...")
    print("=" * 60)
    print()

    # Open live viewer in default browser
    webbrowser.open(live_url)

    # Connect WebSocket viewer in background
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
        print("  (install 'websockets' for live frame stats: pip install websockets)")
        ws = None
        watcher = None
        frames = 0
        actions = []

    # ── Drive action sequence ──────────────────────────────────────────────
    print("driving action sequence:")
    print()

    # Navigate
    print("  [1] navigate to catalogue")
    await send({"id": 2, "method": "navigate", "params": {"url": f"http://127.0.0.1:{PORT}/"}})
    r = await read_json()
    print(f"       -> settled: load={r['result']['load']} network_idle={r['result']['network_idle']}")
    await asyncio.sleep(0.3)

    # Get first book
    print("  [2] get first book title")
    await send({"id": 3, "method": "get_text", "params": {"selector": ".product_pod h3 a"}})
    r = await read_json()
    title = r["result"]["text"]
    print(f"       -> {title}")
    await asyncio.sleep(0.3)

    # Click first book
    print("  [3] click first book")
    await send({"id": 4, "method": "click", "params": {"selector": ".product_pod h3 a"}})
    r = await read_json()
    print(f"       -> ok: {r['result']['ok']}")
    await asyncio.sleep(0.3)

    # Wait for settle
    print("  [4] wait for page to settle")
    await send({"id": 5, "method": "wait_settle", "params": {"timeout_ms": 8000}})
    r = await read_json()
    print(f"       -> load={r['result']['load']} network_idle={r['result']['network_idle']}")
    await asyncio.sleep(0.3)

    # Get detail title
    print("  [5] get detail page title")
    await send({"id": 6, "method": "get_text", "params": {"selector": "h1"}})
    r = await read_json()
    detail_title = r["result"]["text"]
    print(f"       -> {detail_title}")
    await asyncio.sleep(0.3)

    # Get price
    print("  [6] get price")
    await send({"id": 7, "method": "get_text", "params": {"selector": ".price_color"}})
    r = await read_json()
    price = r["result"]["text"]
    print(f"       -> {price}")
    await asyncio.sleep(0.3)

    # Screenshot
    print("  [7] take screenshot")
    await send({"id": 8, "method": "screenshot", "params": {"full_page": True}})
    r = await read_json()
    print(f"       -> {r['result']['len']} bytes, hash={r['result']['hash'][:16]}...")
    await asyncio.sleep(0.3)

    # Observe
    print("  [8] observe page state")
    await send({"id": 9, "method": "observe"})
    r = await read_json()
    obs = r["result"]
    print(f"       -> title={obs['title']}")
    print(f"       -> inputs={len(obs['inputs'])} landmarks={len(obs['landmarks'])} headings={obs['text_blocks']}")
    print(f"       -> state_signature={obs['state_signature'][:16]}...")
    await asyncio.sleep(0.3)

    # ── Summary ────────────────────────────────────────────────────────────
    print()
    print("=" * 60)
    print("SUMMARY")
    print("=" * 60)
    print(f"  book:      {title}")
    print(f"  detail:    {detail_title}")
    print(f"  price:     {price}")
    print(f"  live URL:  {live_url}")
    print("=" * 60)

    # Shutdown
    await send({"id": 10, "method": "shutdown"})

    if watcher:
        await asyncio.sleep(0.5)
        watcher.cancel()
        print(f"\n  live viewer stats: frames={frames} actions={len(actions)}")
        if actions:
            for a in actions:
                print(f"    {a}")

    # Cleanup
    try:
        await asyncio.wait_for(proc.wait(), timeout=5)
    except asyncio.TimeoutError:
        proc.kill()

    try:
        py.terminate()
    except ProcessLookupError:
        pass
    print("\ndemo complete")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
