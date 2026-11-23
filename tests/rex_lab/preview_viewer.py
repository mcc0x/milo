"""A simulated view owner for the disposable Rex lab, never a GUI client."""

import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
import re
import socket
import struct
import sys
import threading
import time


# The Rust fixture passes its own Unix socket, never the ambient Rex endpoint.
timed = len(sys.argv) == 3 and sys.argv[2] == "--bench"

write_lock = threading.Lock()

def now():
    return time.clock_gettime_ns(8)  # Darwin CLOCK_UPTIME_RAW.
with socket.socket(socket.AF_UNIX) as peer:
    peer.settimeout(8)
    peer.connect(sys.argv[1])
    wire = peer.makefile("rb")
    key = base64.b64encode(os.urandom(16)).decode()
    peer.sendall((
        "GET /api/control HTTP/1.1\r\nHost: rex\r\nConnection: Upgrade\r\n"
        "Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n"
    ).encode())
    assert wire.readline().startswith(b"HTTP/1.1 101 ")
    headers = {}
    while (line := wire.readline()) != b"\r\n":
        name, value = line.split(b":", 1)
        headers[name.lower()] = value.strip()
    assert headers[b"sec-websocket-protocol"] == b"rex.control.v1"
    accept = hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
    assert headers[b"sec-websocket-accept"] == base64.b64encode(accept)

    def send(kind, body):
        length = len(body)
        assert length < 65536
        head = bytes([0x80 | kind, 0x80 | (length if length < 126 else 126)])
        if length >= 126:
            head += struct.pack("!H", length)
        mask = os.urandom(4)
        packet = head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(body))
        with write_lock:
            sent = now() if timed else None
            peer.sendall(packet)
        return sent

    def receive():
        while len(header := wire.read(2)) == 2:
            kind, length = header
            assert length < 128  # A server does not mask its frames.
            if length == 126:
                length, = struct.unpack("!H", wire.read(2))
            elif length == 127:
                length, = struct.unpack("!Q", wire.read(8))
            body = wire.read(length)
            if kind == 0x89:
                send(10, body)
                continue
            assert kind == 0x81, (kind, body)  # This peer offers no compression.
            return json.loads(body)
        raise EOFError("Rex lab peer disconnected")

    def send_json(message):
        return send(1, json.dumps(message, separators=(",", ":")).encode())

    def scheduled_resizes(batch, request):
        # Read replies promptly but keep send deadlines independent of Rex's
        # response latency. Both operations still use the same view owner.
        keys = [f"{request}/{i}" for i in range(len(batch))]

        def collect():
            pending = set(keys)
            result = {}
            while pending:
                reply = receive()
                key = reply.get("id")
                if key not in pending:
                    continue
                received = now()
                assert reply["type"] == "response", reply
                match = re.search(r'"revision"\s*:\s*(\d+)', json.dumps(reply))
                assert match, reply
                result[key] = (int(match[1]), received)
                pending.remove(key)
            return result

        with ThreadPoolExecutor(max_workers=1) as pool:
            replies = pool.submit(collect)
            sent = []
            for key, (deadline, payload) in zip(keys, batch):
                while (left := deadline - now()) > 0:
                    time.sleep(left / 1_000_000_000)
                sent.append(send_json({"type": "request", "id": key, "method": "session.resize_pane", "payload": payload}))
            received = replies.result()
        return {"type": "response", "layouts": [
            {"revision": received[key][0], "bench_sent_ns": stamp, "bench_reply_ns": received[key][1]}
            for key, stamp in zip(keys, sent)
        ]}

    send_json({"type": "hello", "api_revision": 18, "client": {
        "kind": "app", "platform": "macos", "name": "Milo Rex Lab (simulated viewer)",
    }})
    welcome = receive()
    assert welcome["type"] == "welcome", welcome
    print(welcome["client"]["id"], flush=True)
    for i, line in enumerate(sys.stdin):
        method, payload = json.loads(line)
        request = str(i)
        if timed and method == "__bench_resize_batch":
            print(json.dumps(scheduled_resizes(payload, request), separators=(",", ":")), flush=True)
            continue
        sent = send_json({"type": "request", "id": request, "method": method, "payload": payload})
        while (reply := receive()).get("id") != request:
            pass
        if timed:
            reply["bench_sent_ns"] = sent
            reply["bench_reply_ns"] = now()
        print(json.dumps(reply, separators=(",", ":")), flush=True)
