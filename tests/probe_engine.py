#!/usr/bin/env python3
"""End-to-end probe for the kiri engine's wire contract.

Boots a fake Anthropic-compatible /v1/messages SSE endpoint, starts the real
engine binary with the exact flags the host uses, drives one successful turn and
one permission-gated tool call, then asserts the recon/architecture contract:

  F01  system:init -> assistant (one per content block) -> result success
  F02  the tool call asks the GUI via control_request:can_use_tool, and the
       GUI's control_response (request_id at the second level) unblocks it
  EOF  closing stdin ends the session cleanly with exit 0

Stdlib only. Run:  python -I tests/probe_engine.py
"""
import json
import os
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TURNS = {"n": 0}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        self.rfile.read(n)
        TURNS["n"] += 1
        turn = TURNS["n"]

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        self.close_connection = True

        if turn == 1:
            # Ask for one write so the permission pipeline, the GUI round trip
            # and the tool executor all run on this turn.
            self.ss("message_start", {"type": "message_start", "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "model": "fake-model", "content": [], "stop_reason": None,
                "usage": {"input_tokens": 10, "output_tokens": 4}}})
            self.ss("content_block_start", {
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "tu1",
                                  "name": "Write", "input": {}}})
            payload = json.dumps({"file_path": "probe_note.txt", "content": "hi"})
            self.ss("content_block_delta", {
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": payload}})
            self.ss("content_block_stop", {"type": "content_block_stop", "index": 0})
            self.ss("message_delta", {"type": "message_delta",
                                      "delta": {"stop_reason": "tool_use"},
                                      "usage": {"output_tokens": 4}})
        else:
            self.ss("message_start", {"type": "message_start", "message": {
                "id": "msg_2", "type": "message", "role": "assistant",
                "model": "fake-model", "content": [], "stop_reason": None,
                "usage": {"input_tokens": 20, "output_tokens": 1}}})
            self.ss("content_block_start", {
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}})
            self.ss("content_block_delta", {
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "DONE"}})
            self.ss("content_block_stop", {"type": "content_block_stop", "index": 0})
            self.ss("message_delta", {"type": "message_delta",
                                      "delta": {"stop_reason": "end_turn"},
                                      "usage": {"output_tokens": 1}})
        self.ss("message_stop", {"type": "message_stop"})
        self.wfile.flush()

    def ss(self, event, data):
        self.wfile.write(("event: %s\ndata: %s\n\n" % (event, json.dumps(data))).encode())
        self.wfile.flush()


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = os.path.join(root, "engine", "target", "debug", "kiri-engine.exe")
    work = os.path.join(root, "tests", "_work")
    os.makedirs(work, exist_ok=True)

    env = dict(os.environ)
    env["ANTHROPIC_API_KEY"] = "probe-key"
    env["KIRI_BASE_URL"] = "http://127.0.0.1:%d" % port
    env["KIRI_MODEL"] = "fake-model"

    prompt = {"type": "user", "message": {"role": "user",
              "content": [{"type": "text", "text": "write a note"}]}}
    proc = subprocess.Popen(
        [exe, "--input-format", "stream-json", "--output-format", "stream-json",
         "--verbose", "--cwd", work, "--permission-mode", "default"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", env=env, cwd=root,
    )
    proc.stdin.write(json.dumps(prompt) + "\n")
    proc.stdin.flush()

    lines = []
    answered = False
    deadline = time.time() + 40
    while time.time() < deadline:
        line = proc.stdout.readline()
        if not line:
            break
        v = json.loads(line)
        lines.append(v)
        if v.get("type") == "control_request" and v.get("request", {}).get("subtype") == "can_use_tool":
            proc.stdin.write(json.dumps({
                "type": "control_response",
                "response": {"subtype": "success",
                             "request_id": v["request_id"],
                             "response": {"behavior": "allow"}},
            }) + "\n")
            proc.stdin.flush()
            answered = True
        if v.get("type") == "result" and v.get("subtype") == "success":
            break
    proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        print("FAIL  engine did not exit after stdin EOF")
        return 1

    kinds = [(v.get("type"), v.get("subtype")) for v in lines]

    def has(t, s=None):
        return any(k[0] == t and (s is None or k[1] == s) for k in kinds)

    checks = [
        ("system:init emitted", has("system", "init")),
        ("assistant content block streamed", has("assistant")),
        ("tool_use assistant block carried the call", any(
            v.get("type") == "assistant"
            and any(b.get("type") == "tool_use" and b.get("name") == "Write"
                    for b in v.get("message", {}).get("content", []))
            for v in lines)),
        ("permission prompt reached the GUI", answered),
        ("tool_result returned as user message", has("user")),
        ("final assistant text is DONE", any(
            v.get("type") == "assistant"
            and any(b.get("type") == "text" and b.get("text") == "DONE"
                    for b in v.get("message", {}).get("content", []))
            for v in lines)),
        ("result:success closes the turn", has("result", "success")),
        ("process exited 0", proc.returncode == 0),
    ]
    ok = True
    for name, good in checks:
        print(("PASS  " if good else "FAIL  ") + name)
        ok = ok and good
    if not ok:
        print("\n--- stream ---")
        for v in lines:
            print(json.dumps(v)[:400])
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
