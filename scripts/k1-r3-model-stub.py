#!/usr/bin/env python3
"""A scripted local model for scripts/k1-r3-accept.sh (loopback only).

    k1-r3-model-stub.py <port> <dump dir> <script.json>

The daemon reaches it as OMLX_URL (an OpenAI-style chat endpoint). The script
maps a marker to steps: {"MARKER": [{"call": {"name": ..., "arguments": {...}}},
..., {"say": "text"}]}. A request belongs to the marker its last user message
holds (a session run also carries the earlier turns); the step is the number
of tool-calling assistant turns since that message.
A "call" step whose tool the request does not offer answers
"NOT-OFFERED: <name>" instead. A "say" step's text has "{last}" replaced by the
last tool answer, and "{all}" by every tool answer of the turn joined by
" | ". Past the last step, or with no marker, it says "STUB-DONE".
Every POST body is saved as <dump dir>/<n>.json. GET /v1/models lists one model.
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port, dump, script_file = int(sys.argv[1]), sys.argv[2], sys.argv[3]
lock = threading.Lock()
seen = 0


def completion(message, finish):
    return {
        "id": "k1r3-stub", "object": "chat.completion", "model": "k1r3-stub",
        "choices": [{"index": 0, "finish_reason": finish, "message": message}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


def reply(req, n):
    with open(script_file) as f:
        script = json.load(f)
    messages = req.get("messages") or []
    # A session run carries the session's earlier turns: only this turn counts
    users = [i for i, m in enumerate(messages) if m.get("role") == "user"]
    turn = messages[users[-1]:] if users else []
    text = str(turn[0].get("content")) if turn else ""
    steps = next((s for marker, s in script.items() if marker in text), [])
    done = sum(1 for m in turn if m.get("role") == "assistant" and m.get("tool_calls"))
    tools = [m for m in turn if m.get("role") == "tool"]
    last = str(tools[-1].get("content")) if tools else ""
    step = steps[done] if done < len(steps) else {"say": "STUB-DONE"}
    if "call" in step:
        offered = [t.get("function", {}).get("name") for t in req.get("tools") or []]
        name = step["call"]["name"]
        if name not in offered:
            return completion({"role": "assistant", "content": f"NOT-OFFERED: {name}"}, "stop")
        return completion({
            "role": "assistant", "content": None,
            "tool_calls": [{
                "id": f"call_k1r3_{n}", "type": "function",
                "function": {"name": name, "arguments": json.dumps(step["call"]["arguments"])},
            }],
        }, "tool_calls")
    every = " | ".join(str(m.get("content")) for m in tools)
    return completion({"role": "assistant", "content": step["say"].replace("{last}", last).replace("{all}", every)}, "stop")


class Handler(BaseHTTPRequestHandler):
    def answer(self, body):
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.answer({"object": "list", "data": [{"id": "k1r3-stub", "object": "model"}]})

    def do_POST(self):
        global seen
        raw = self.rfile.read(int(self.headers.get("content-length") or 0))
        with lock:
            seen += 1
            n = seen
        with open(f"{dump}/{n:03d}.json", "wb") as f:
            f.write(raw)
        self.answer(reply(json.loads(raw or b"{}"), n))

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
