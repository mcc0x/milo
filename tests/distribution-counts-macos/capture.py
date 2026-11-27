import argparse
import datetime
from http.server import BaseHTTPRequestHandler, HTTPServer
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import ssl

p = argparse.ArgumentParser()
p.add_argument("host")
p.add_argument("chain", type=Path)
p.add_argument("key", type=Path)
p.add_argument("ledger", type=Path)
p.add_argument("--fixture-preflight-port", type=int)
args = p.parse_args()
assert re.fullmatch(r"(?:[A-Za-z0-9-]+\.)+[A-Za-z0-9-]+", args.host)
os.umask(0o077)
fixture_only = args.fixture_preflight_port is not None
port = args.fixture_preflight_port if fixture_only else 443
if fixture_only:
    assert args.host.endswith(".test") and port > 1024
else:
    answers = {ipaddress.ip_address(r[4][0]) for r in socket.getaddrinfo(args.host, port, type=socket.SOCK_STREAM)}
    assert answers == {ipaddress.ip_address("127.0.0.1")}
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.minimum_version = ssl.TLSVersion.TLSv1_2
context.set_alpn_protocols(["http/1.1"])
context.load_cert_chain(args.chain,args.key)

def verify_sni(_socket, host, _context):
    return None if host == args.host else ssl.ALERT_DESCRIPTION_UNRECOGNIZED_NAME

context.sni_callback=verify_sni
with args.ledger.open("x", encoding="utf-8") as ledger:
    class Receiver(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        def log_message(self,*unused):
            pass
        def do_POST(self):
            limit = self.headers.get("Content-Length", "")
            expected_host = f"{args.host}:{port}" if fixture_only else args.host
            keys = [k.lower() for k in self.headers.keys()]
            expected = {"host","content-type","content-length","accept"}
            header_ok = (self.request_version == "HTTP/1.1" and self.path == "/v1/counts"
                         and self.headers.get("Host") == expected_host
                         and self.headers.get("Content-Type") == "application/json"
                         and limit.isdecimal() and 0 < int(limit) <= 128
                         and len(keys) == len(set(keys)) and set(keys) == expected
                         and self.headers.get("Accept") == "*/*")
            clean = False
            if header_ok:
                self.connection.settimeout(3)
                try:
                    raw=self.rfile.read(int(limit))
                    items=json.loads(raw,object_pairs_hook=tuple)
                    clean=(isinstance(items,tuple) and sorted(k for k,v in items)==["day","monthly_active","new_install"])
                    if clean:
                        body=dict(items)
                        day=body["day"]
                        clean=(isinstance(day,str) and bool(re.fullmatch(r"\d{4}-\d{2}-\d{2}",day))
                               and datetime.date.fromisoformat(day).isoformat()==day
                               and all(type(body[k]) is bool for k in ("new_install","monthly_active")))
                except (OSError,ValueError,TypeError,UnicodeError):
                    clean=False
            if clean:
                event={"event":"accepted","tls":self.connection.version(),"day":day,
                       "monthly_active":body["monthly_active"],"new_install":body["new_install"]}
            else:
                event={"event":"rejected"}
            ledger.write(json.dumps(event,separators=(",",":"))+"\n")
            ledger.flush()
            self.send_response_only(204 if clean else 400)
            self.send_header("Connection","close")
            self.send_header("Content-Length","0")
            self.end_headers()
            self.close_connection = True

    with HTTPServer(("127.0.0.1",port),Receiver) as server:
        server.socket=context.wrap_socket(server.socket,server_side=True,do_handshake_on_connect=False)
        print("ready",flush=True)
        server.serve_forever()
