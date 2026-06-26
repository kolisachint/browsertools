# Live-view check: drives tool calls through `serve` while a WebSocket
# client watches the viewer, asserting frames + synced action events.
# Prereqs: build the binary, and serve fixtures/site on :8732, e.g.
#   python3 -m http.server 8732 --directory fixtures/site &
#   cargo build && python3 scripts/live_view_check.py

import asyncio, json, subprocess, sys, base64, os

import os
BIN = os.environ.get("BT_BIN", "target/debug/browsertools")

async def main():
    proc = await asyncio.create_subprocess_exec(
        BIN, "serve",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
    )

    async def send(obj):
        proc.stdin.write((json.dumps(obj) + "\n").encode())
        await proc.stdin.drain()

    async def read_json():
        line = await proc.stdout.readline()
        return json.loads(line.decode())

    # ready event
    ev = await read_json()
    assert ev.get("event") == "ready", ev

    # start live view
    await send({"id": 1, "method": "live_view_start"})
    resp = await read_json()
    url = resp["result"]["url"]
    devtools = resp["result"].get("devtools_ws", "")
    print(f"live_view url = {url}")
    print(f"devtools fallback = {devtools[:48]}...")

    import websockets
    ws_url = url.replace("http://", "ws://") + "ws"

    frames = 0
    actions = []
    last_frame = None

    async with websockets.connect(ws_url) as ws:
        async def watch():
            nonlocal frames, last_frame
            try:
                async for raw in ws:
                    m = json.loads(raw)
                    if m["type"] == "frame":
                        frames += 1
                        last_frame = m["data"]
                    elif m["type"] == "action":
                        actions.append(m["text"])
                        print("  WS action:", m["text"])
            except Exception:
                pass

        watcher = asyncio.create_task(watch())

        # Drive tool calls; watch the action events arrive over the socket.
        await send({"id": 2, "method": "navigate", "params": {"url": "http://127.0.0.1:8732/"}})
        await read_json()
        await asyncio.sleep(0.6)
        await send({"id": 3, "method": "click", "params": {"selector": ".product_pod h3 a"}})
        await read_json()
        await send({"id": 4, "method": "wait_settle", "params": {"timeout_ms": 6000}})
        await read_json()
        await asyncio.sleep(0.6)
        await send({"id": 5, "method": "get_text", "params": {"selector": "h1"}})
        r = await read_json()
        print("  detail h1 =", r["result"]["text"])
        await asyncio.sleep(0.6)

        watcher.cancel()

    await send({"id": 9, "method": "shutdown"})
    try:
        await asyncio.wait_for(proc.wait(), timeout=10)
    except asyncio.TimeoutError:
        proc.kill()

    if last_frame:
        with open("/tmp/live_frame.png", "wb") as f:
            f.write(base64.b64decode(last_frame))  # actually jpeg bytes
        os.replace("/tmp/live_frame.png", "/tmp/live_frame.jpg")

    print(f"\nRESULT: frames_received={frames} actions_received={len(actions)}")
    print("actions:", actions)
    ok = frames > 0 and any("navigate" in a for a in actions) and any("click" in a for a in actions)
    print("LIVE_VIEW_OK" if ok else "LIVE_VIEW_FAIL")
    sys.exit(0 if ok else 1)

asyncio.run(main())
