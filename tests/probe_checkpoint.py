#!/usr/bin/env python3
"""Prove the engine writes a `file_checkpoint` before Write/Edit mutates a file.

Fake SSE endpoint asks for one Write; the engine must record the pre-edit body
first. Then a second Write on the same file must record the new pre-edit body.
"""
import json, os, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TURNS = {"n": 0}

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def ss(self, event, data):
        self.wfile.write(("event: %s\ndata: %s\n\n" % (event, json.dumps(data))).encode()); self.wfile.flush()
    def do_POST(self):
        n = int(self.headers.get("content-length", "0")); self.rfile.read(n)
        TURNS["n"] += 1; turn = TURNS["n"]
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close"); self.end_headers(); self.close_connection = True
        self.ss("message_start", {"type": "message_start", "message": {"id": "m%d" % turn,
            "type": "message", "role": "assistant", "model": "fake-model", "content": [],
            "stop_reason": None, "usage": {"input_tokens": 10, "output_tokens": 2}}})
        if turn == 1:
            self.ss("content_block_start", {"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "w1", "name": "Write", "input": {}}})
            body = json.dumps({"file_path": "note.txt", "content": "first"})
            self.ss("content_block_delta", {"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": body}})
        elif turn == 2:
            self.ss("content_block_start", {"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "w2", "name": "Write", "input": {}}})
            body = json.dumps({"file_path": "note.txt", "content": "second"})
            self.ss("content_block_delta", {"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": body}})
        else:
            self.ss("content_block_start", {"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}})
            self.ss("content_block_delta", {"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "DONE"}})
        self.ss("content_block_stop", {"type": "content_block_stop", "index": 0})
        self.ss("message_delta", {"type": "message_delta",
            "delta": {"stop_reason": "tool_use" if turn < 3 else "end_turn"},
            "usage": {"output_tokens": 2}})
        self.ss("message_stop", {"type": "message_stop"})


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = os.path.join(root, "engine", "target", "debug", "kiri-engine.exe")
    work = os.path.join(root, "tests", "_work_cp")
    os.makedirs(work, exist_ok=True)
    try:
        os.remove(os.path.join(work, "note.txt"))
    except OSError:
        pass
    env = dict(os.environ)
    env["ANTHROPIC_API_KEY"] = "probe-key"
    env["KIRI_BASE_URL"] = "http://127.0.0.1:%d" % port
    env["KIRI_MODEL"] = "fake-model"

    sid = "probe-session-2"
    prompt = {"type": "user", "message": {"role": "user",
              "content": [{"type": "text", "text": "write it twice"}]}}
    proc = subprocess.Popen(
        [exe, "--input-format", "stream-json", "--output-format", "stream-json",
         "--verbose", "--cwd", work, "--permission-mode", "bypassPermissions",
         "--session-id", sid],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", env=env, cwd=root,
    )
    proc.stdin.write(json.dumps(prompt) + "\n"); proc.stdin.flush()
    deadline = time.time() + 40
    while time.time() < deadline:
        line = proc.stdout.readline()
        if not line:
            break
        v = json.loads(line)
        if v.get("type") == "result":
            break
    proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()

    home = os.path.join(os.path.expanduser("~"), ".kiri", "projects")
    matches = []
    for dirpath, _d, files in os.walk(home):
        for f in files:
            if f == sid + ".jsonl":
                matches.append(os.path.join(dirpath, f))
    if not matches:
        print("FAIL  no transcript written for", sid)
        return 1
    path = matches[0]
    cps = []
    for raw in open(path, encoding="utf-8"):
        raw = raw.strip()
        if not raw:
            continue
        try:
            e = json.loads(raw)
        except Exception:
            continue
        if e.get("type") == "file_checkpoint":
            cps.append(e)
    expect = [
        ("first", False),   # file did not exist before Write #1
        ("second", True),   # existed, body was "first"
    ]
    got = []
    for c in cps:
        body = c.get("body_base64")
        text = None
        if body:
            import base64
            text = base64.b64decode(body).decode()
        got.append((text, bool(c.get("existedPre"))))
    ok = True
    print("checkpoints seen:", len(got))
    for i, (want_text, want_existed) in enumerate(expect):
        have = got[i] if i < len(got) else None
        good = have is not None and have[1] == want_existed and (
            (want_text == "first" and not want_existed) or have[0] == "first"
        )
        print(("PASS  " if good else "FAIL  ") + "checkpoint %d %r" % (i, have))
        ok = ok and good
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
