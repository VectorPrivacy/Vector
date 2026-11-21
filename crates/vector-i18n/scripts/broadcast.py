#!/usr/bin/env python3
"""Hands the signed index events to relays, so a network that can't reach the release can still
find them: `broadcast.py index.json wss://relay …`. A refusal is a warning, never a failure: the
release stays the source, and the app checks every signature itself."""

import asyncio
import json
import sys

import websockets


async def send(url, events):
    answers = {}
    try:
        async with websockets.connect(url, open_timeout=15, close_timeout=5) as ws:
            for ev in events:
                await ws.send(json.dumps(["EVENT", ev]))

            async def read():
                while len(answers) < len(events):
                    msg = json.loads(await ws.recv())
                    if isinstance(msg, list) and len(msg) >= 3 and msg[0] == "OK":
                        answers[msg[1]] = (msg[2] is True, msg[3] if len(msg) > 3 else "")

            await asyncio.wait_for(read(), 15)
    except Exception as e:  # noqa: BLE001 - every failure is reported the same way
        if not answers:
            print(f"::warning::{url}: {e}")
            return
    for ev in events:
        accepted, why = answers.get(ev["id"], (False, "no answer"))
        if accepted:
            print(f"{url}: {ev['id'][:12]} accepted")
        else:
            print(f"::warning::{url}: {ev['id'][:12]} refused: {why}")


async def main():
    if len(sys.argv) < 3:
        sys.exit("usage: broadcast.py index.json wss://relay …")
    with open(sys.argv[1]) as f:
        events = json.load(f)
    await asyncio.gather(*(send(url, events) for url in sys.argv[2:]))


asyncio.run(main())
