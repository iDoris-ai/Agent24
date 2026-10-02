#!/usr/bin/env python3
import json
import os
import select
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


replies = []


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def send_json(self, payload):
        encoded = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def do_GET(self):
        if self.path == "/v1/models":
            self.send_json({"data": [{"id": "m4-stub"}]})
            return
        self.send_error(404)

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        replies.append(body)
        turn = len(replies)
        self.send_json(
            {
                "model": "m4-stub",
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": f"m4-turn-{turn}",
                            "tool_calls": None,
                            "tool_call_id": None,
                        }
                    }
                ],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": 2,
                    "total_tokens": 5,
                },
            }
        )


def send(proc, request):
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()


def receive_for(proc, request_id, timeout=90):
    deadline = time.monotonic() + timeout
    frames = []
    while time.monotonic() < deadline:
        ready, _, _ = select.select([proc.stdout], [], [], 1.0)
        if not ready:
            if proc.poll() is not None:
                raise RuntimeError(
                    f"ACP exited early rc={proc.returncode}: {proc.stderr.read()}"
                )
            continue
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError(f"ACP stdout closed: {proc.stderr.read()}")
        frame = json.loads(line)
        frames.append(frame)
        if frame.get("id") == request_id:
            if "error" in frame:
                raise RuntimeError(f"ACP request {request_id} failed: {frame}")
            return frame, frames
    raise TimeoutError(
        f"timed out waiting for ACP response id={request_id}; frames={frames}"
    )


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    env = os.environ.copy()
    env.update(
        {
            "AGENT24D_BIN": os.environ["AGENT24D_BIN"],
            "OMLX_URL": f"http://127.0.0.1:{server.server_port}",
            "OMLX_API_KEY": "",
            "DEFAULT_MODEL": "m4-stub",
        }
    )
    proc = subprocess.Popen(
        [os.environ["AGENT24_BIN"], "acp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )

    send(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    init, _ = receive_for(proc, 1)
    assert init["result"]["protocolVersion"] == 1, init

    send(
        proc,
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"title": "M10 M4 exact-SHA"},
        },
    )
    created, _ = receive_for(proc, 2)
    session_id = created["result"]["sessionId"]
    assert session_id

    def prompt(request_id, text):
        send(
            proc,
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "session/prompt",
                "params": {
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": text}],
                },
            },
        )
        response, frames = receive_for(proc, request_id)
        assert response["result"]["stopReason"] == "end_turn", response
        chunks = [
            frame["params"]["update"]["content"]["text"]
            for frame in frames
            if frame.get("method") == "session/update"
            and frame.get("params", {}).get("sessionId") == session_id
        ]
        return "".join(chunks)

    first = prompt(3, "first M4 turn")
    second = prompt(4, "second M4 turn")
    assert first == "m4-turn-1", first
    assert second == "m4-turn-2", second
    assert len(replies) == 2, replies

    proc.stdin.close()
    proc.wait(timeout=20)
    if proc.returncode != 0:
        raise RuntimeError(f"ACP exit rc={proc.returncode}: {proc.stderr.read()}")
    server.shutdown()

    print(
        json.dumps(
            {
                "sessionId": session_id,
                "turns": 2,
                "first": first,
                "second": second,
                "providerCalls": len(replies),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
