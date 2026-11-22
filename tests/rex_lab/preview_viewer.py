"""A simulated view owner for the disposable Rex lab, never a GUI client."""

import base64
import hashlib
import json
import os
import socket
import struct
import sys


# The Rust fixture passes its own Unix socket, never the ambient Rex endpoint.
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
        peer.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(body)))

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
        send(1, json.dumps(message, separators=(",", ":")).encode())

    send_json({"type": "hello", "api_revision": 18, "client": {
        "kind": "app", "platform": "macos", "name": "Milo Rex Lab (simulated viewer)",
    }})
    welcome = receive()
    assert welcome["type"] == "welcome", welcome
    print(welcome["client"]["id"], flush=True)
    for i, line in enumerate(sys.stdin):
        method, payload = json.loads(line)
        request = str(i)
        send_json({"type": "request", "id": request, "method": method, "payload": payload})
        while (reply := receive()).get("id") != request:
            pass
        print(json.dumps(reply, separators=(",", ":")), flush=True)
