"""A loopback stand-in for an OpenAI-compatible model: answers every request with a fixed echo.

usage: standin.py PORT LOGFILE
Serves /v1/chat/completions (stream or not), /v1/responses (stream or not) and /v1/models on 127.0.0.1 only.
Every request body is appended to LOGFILE as one JSON line, so a test can see the prompt exactly as it arrived.
The reply is `STANDIN heard: <last user text, first 120 chars, newlines as spaces>`.
"""
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT, LOG = int(sys.argv[1]), sys.argv[2]

def text_of(content):
    if isinstance(content, str): return content
    if isinstance(content, list):
        for part in reversed(content):
            if isinstance(part, dict) and part.get("type") in ("text", "input_text") and part.get("text", "").strip():
                return part["text"]
    return ""

def last_user(body):
    items = body.get("messages") or body.get("input") or []
    if isinstance(items, str): return items
    for item in reversed(items):
        if isinstance(item, dict) and item.get("role") == "user":
            said = text_of(item.get("content"))
            if said.strip(): return said
    return ""

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def send_json(self, obj, code=200):
        data = json.dumps(obj).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def sse(self, events):
        self.send_response(200); self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache"); self.send_header("Connection", "close"); self.end_headers()
        for name, data in events:
            if name: self.wfile.write(f"event: {name}\n".encode())
            self.wfile.write(("data: " + (data if isinstance(data, str) else json.dumps(data)) + "\n\n").encode())
            self.wfile.flush()
        self.close_connection = True
    def do_GET(self):
        with open(LOG, "a") as f: f.write(json.dumps({"path": self.path, "method": "GET"}) + "\n")
        if self.path.rstrip("/").endswith("/models"):
            return self.send_json({"object": "list", "data": [{"id": "stand-in", "object": "model", "owned_by": "milo-test"}]})
        self.send_json({"error": "not found"}, 404)
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        try: body = json.loads(raw)
        except ValueError: body = {}
        with open(LOG, "a") as f: f.write(json.dumps({"path": self.path, "body": body}) + "\n")
        reply = "STANDIN heard: " + " ".join(last_user(body).split())[:120]
        model, now, usage = body.get("model", "stand-in"), int(time.time()), {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}
        if self.path.rstrip("/").endswith("/chat/completions"):
            if body.get("stream"):
                base = {"id": "chatcmpl-standin", "object": "chat.completion.chunk", "created": now, "model": model}
                return self.sse([
                    (None, {**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": None}]}),
                    (None, {**base, "choices": [{"index": 0, "delta": {"content": reply}, "finish_reason": None}]}),
                    (None, {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                    (None, {**base, "choices": [], "usage": usage}),
                    (None, "[DONE]")])
            return self.send_json({"id": "chatcmpl-standin", "object": "chat.completion", "created": now, "model": model,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": reply}, "finish_reason": "stop"}], "usage": usage})
        if self.path.rstrip("/").endswith("/responses"):
            item = {"id": "msg_standin", "type": "message", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": reply, "annotations": []}]}
            done = {"id": "resp_standin", "object": "response", "created_at": now, "status": "completed", "model": model, "output": [item],
                    "usage": {"input_tokens": 12, "output_tokens": 5, "total_tokens": 17}}
            if body.get("stream"):
                return self.sse([
                    ("response.created", {"type": "response.created", "response": {**done, "status": "in_progress", "output": []}}),
                    ("response.output_item.added", {"type": "response.output_item.added", "output_index": 0, "item": {**item, "status": "in_progress", "content": []}}),
                    ("response.content_part.added", {"type": "response.content_part.added", "item_id": "msg_standin", "output_index": 0, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}),
                    ("response.output_text.delta", {"type": "response.output_text.delta", "item_id": "msg_standin", "output_index": 0, "content_index": 0, "delta": reply}),
                    ("response.output_text.done", {"type": "response.output_text.done", "item_id": "msg_standin", "output_index": 0, "content_index": 0, "text": reply}),
                    ("response.content_part.done", {"type": "response.content_part.done", "item_id": "msg_standin", "output_index": 0, "content_index": 0, "part": item["content"][0]}),
                    ("response.output_item.done", {"type": "response.output_item.done", "output_index": 0, "item": item}),
                    ("response.completed", {"type": "response.completed", "response": done})])
            return self.send_json(done)
        self.send_json({"error": {"message": "stand-in: unknown path " + self.path}}, 404)

ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
