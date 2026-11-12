#!/usr/bin/env python3
"""The transcript must carry the side records the GUI screens read.

S06 (rules) reads permission_request rows, S07 the question card payload
and S08 file_checkpoint rows. The engine writes them beside the message lines;
this asserts each appears, so the host indexer and the file tailer both get data.

Run:  python -I tests/probe_side.py
"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TURNS = {"n": 0}


def tool_use(call_id, name, args):
    return {"type": "tool_use", "id": call_id, "name": name, "input": args}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def ss(self, event, data):
        data = dict(data)
        data["type"] = event
        line = "event: %s\ndata: %s\n\n" % (event, json.dumps(data))
        self.wfile.write(line.encode())
        self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        self.rfile.read(n)
        TURNS["n"] += 1
        turn = TURNS["n"]

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        self.close_connection = True

        self.ss("message_start", {"message": {
            "id": "m%d" % turn,
            "type": "message",
            "role": "assistant",
            "model": "fake",
            "content": [],
            "stop_reason": None,
            "usage": {"input_tokens": 5, "output_tokens": 2},
        }})

        if turn == 1:
            block = tool_use("w1", "Write", {})
            payload = {"file_path": "n.txt", "content": "x"}
            stop = "tool_use"
        elif turn == 2:
            block = tool_use("q1", "AskUserQuestion", {})
            payload = {"questions": [{
                "question": "which?", "header": "pick",
                "options": [{"label": "a"}, {"label": "b"}],
            }]}
            stop = "tool_use"
        else:
            block = {"type": "text", "text": ""}
            payload = None
            stop = "end_turn"

        self.ss("content_block_start", {"index": 0, "content_block": block})
        if payload is not None:
            self.ss("content_block_delta", {"index": 0, "delta": {
                "type": "input_json_delta",
                "partial_json": json.dumps(payload),
            }})
        else:
            self.ss("content_block_delta", {"index": 0, "delta": {
                "type": "text_delta", "text": "DONE",
            }})
        self.ss("content_block_stop", {"index": 0})
        self.ss("message_delta", {
            "delta": {"stop_reason": stop},
            "usage": {"output_tokens": 2},
        })
        self.ss("message_stop", {})


def slug(path):
    return "".join(c if c.isalnum() or c in "-_." else "-" for c in path)


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = os.path.join(root, "engine", "target", "debug", "kiri-engine.exe")
    work = tempfile.mkdtemp(prefix="kiri-side-")
    env = dict(os.environ)
    # The ambient shell may define these; a leftover would send the probe to a
    # real service with real credentials. Strip them, then set the probe's own.
    env.pop("ANTHROPIC_BASE_URL", None)
    env.pop("ANTHROPIC_AUTH_TOKEN", None)
    env.pop("ANTHROPIC_API_KEY", None)
    env["ANTHROPIC_API_KEY"] = "probe-key"
    env["KIRI_BASE_URL"] = "http://127.0.0.1:%d" % port
    env["KIRI_MODEL"] = "fake"

    sid = "side-records-1"
    # `default`, not `acceptEdits`: acceptEdits auto-allows Write, so
    # ask_permission never runs and no permission_request line is written.
    proc = subprocess.Popen(
        [exe, "--input-format", "stream-json", "--output-format", "stream-json",
         "--cwd", work, "--session-id", sid, "--permission-mode", "default"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", env=env,
    )
    proc.stdin.write(json.dumps({
        "type": "user",
        "message": {"role": "user", "content": [{"type": "text", "text": "go"}]},
    }) + "\n")
    proc.stdin.flush()

    # Answer the permission prompts in the main loop, exactly as the GUI does;
    # the engine blocks its turn until it receives the control_response.
    # Read until `result` (or the deadline): a fixed line cap would cut the
    # turn off before the question is answered and the checkpoint written.
    seen = []
    deadline = time.time() + 30
    while time.time() < deadline:
        line = proc.stdout.readline()
        if not line:
            break
        seen.append(line.strip())
        try:
            v = json.loads(line)
        except Exception:
            continue
        req = v.get("request", {})
        if v.get("type") == "control_request" and req.get("subtype") == "can_use_tool":
            proc.stdin.write(json.dumps({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": v["request_id"],
                    "response": {"behavior": "allow"},
                },
            }) + "\n")
            proc.stdin.flush()
        if v.get("type") == "result":
            break

    proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()

    # The engine canonicalises its cwd and names the transcript dir after it.
    # Use the cwd it reported in `system:init`, which is exactly that value.
    eng_cwd = None
    for row in seen:
        try:
            o = json.loads(row)
        except Exception:
            continue
        if o.get("type") == "system" and o.get("subtype") == "init":
            eng_cwd = o.get("cwd")
            break
    roots = [r for r in (eng_cwd, work) if r]
    path = None
    for r in roots:
        cand = os.path.join(os.path.expanduser("~/.kiri/projects"), slug(r), sid + ".jsonl")
        if os.path.exists(cand):
            path = cand
            break
    if path is None:
        print("FAIL  no transcript; tried:")
        for r in roots:
            print("   ", os.path.join(os.path.expanduser("~/.kiri/projects"), slug(r), sid + ".jsonl"))
        for s in seen:
            print("  stdout:", s[:400])
        return 1

    kinds = []
    for raw in open(path, encoding="utf-8"):
        raw = raw.strip()
        if raw:
            try:
                kinds.append(json.loads(raw).get("type"))
            except Exception:
                pass

    for s2 in seen:
        print("SEEN", s2[:300])
    ok = True
    for k in ["permission_request", "question", "file_checkpoint"]:
        good = k in kinds
        print(("PASS  " if good else "FAIL  ") + k)
        ok = ok and good
    if not ok:
        print("types:", sorted(set(kinds)))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
