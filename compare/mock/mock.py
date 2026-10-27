"""Mock outbound targets for the Python vs Rust comparison.

One process, four listeners:
  :8080  HTTP   every outbound webhook (Discord, Slack, Teams, Telegram, ntfy,
                generic, legacy forward_url) and, for Rust, Turnstile, Resend
                and SendGrid through HOOKFORMS_TEST_UPSTREAM_BASE
  :443   HTTPS  the same handler, for Python, whose Turnstile, Resend and
                SendGrid hosts are mapped to this container (test CA)
  :2525  SMTP   plain SMTP capture
  :8081  HTTP   control: GET /_records, POST /_reset

Every request is recorded with its source IP, so the harness can tell which
app sent it.
"""

import email
import email.policy
import json
import socketserver
import ssl
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qsl, urlsplit

RECORDS = []
LOCK = threading.Lock()


def record(item):
    with LOCK:
        RECORDS.append(item)


class Target(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _handle(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        parts = urlsplit(self.path)
        text = raw.decode("utf-8", errors="replace")
        try:
            parsed = json.loads(text) if text else None
        except ValueError:
            parsed = None
        form = None
        ctype = self.headers.get("content-type", "")
        if "application/x-www-form-urlencoded" in ctype:
            form = parse_qsl(text, keep_blank_values=True)
        record({
            "proto": "https" if isinstance(self.connection, ssl.SSLSocket) else "http",
            "src": self.client_address[0],
            "method": self.command,
            "path": parts.path,
            "query": parts.query,
            "headers": {k.lower(): v for k, v in self.headers.items()},
            "body": text if parsed is None and form is None else None,
            "json": parsed,
            "form": form,
        })
        status, body, extra = 200, b'{"ok":true}', {"Content-Type": "application/json"}
        if parts.path == "/turnstile/v0/siteverify":
            token = dict(form or []).get("response", "")
            if token == "notjson-token":
                body, extra = b"not json", {"Content-Type": "text/plain"}
            else:
                body = json.dumps({"success": token == "good-token"}).encode()
        elif parts.path.endswith("/status500"):
            status = 500
        elif parts.path.endswith("/redirect"):
            status, body, extra = 302, b"", {"Location": "/landed"}
        self.send_response(status)
        for k, v in extra.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = do_HEAD = do_OPTIONS = _handle


class Control(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        with LOCK:
            body = json.dumps(RECORDS).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        with LOCK:
            RECORDS.clear()
        self.send_response(204)
        self.end_headers()


def parse_message(data: bytes):
    msg = email.message_from_bytes(data, policy=email.policy.default)
    body = msg.get_body(preferencelist=("html", "plain"))
    return {
        "subject": str(msg.get("subject", "")),
        "from": str(msg.get("from", "")),
        "to": str(msg.get("to", "")),
        "content_type": msg.get_content_type(),
        "html": body.get_content() if body is not None else None,
    }


class Smtp(socketserver.StreamRequestHandler):
    def reply(self, line):
        self.wfile.write(line.encode() + b"\r\n")
        self.wfile.flush()

    def handle(self):
        self.reply("220 mock ESMTP")
        mail_from, rcpt = None, []
        while True:
            line = self.rfile.readline()
            if not line:
                return
            cmd = line.decode(errors="replace").strip()
            up = cmd.upper()
            if up.startswith("EHLO"):
                self.wfile.write(b"250-mock\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n")
                self.wfile.flush()
            elif up.startswith("HELO"):
                self.reply("250 mock")
            elif up.startswith("MAIL FROM"):
                mail_from = cmd[10:].strip()
                self.reply("250 OK")
            elif up.startswith("RCPT TO"):
                rcpt.append(cmd[8:].strip())
                self.reply("250 OK")
            elif up == "DATA":
                self.reply("354 go")
                buf = []
                while True:
                    l = self.rfile.readline()
                    if not l or l in (b".\r\n", b".\n"):
                        break
                    if l.startswith(b".."):
                        l = l[1:]
                    buf.append(l)
                data = b"".join(buf)
                item = {"proto": "smtp", "src": self.client_address[0],
                        "mail_from": mail_from, "rcpt_to": rcpt}
                try:
                    item.update(parse_message(data))
                except Exception as exc:  # keep the record even if parsing fails
                    item["parse_error"] = repr(exc)
                record(item)
                mail_from, rcpt = None, []
                self.reply("250 queued")
            elif up == "QUIT":
                self.reply("221 bye")
                return
            elif up.startswith("RSET"):
                mail_from, rcpt = None, []
                self.reply("250 OK")
            else:
                self.reply("250 OK")


class ThreadingTCP(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    cert, key = sys.argv[1], sys.argv[2]
    servers = [
        ThreadingHTTPServer(("0.0.0.0", 8080), Target),
        ThreadingHTTPServer(("0.0.0.0", 8081), Control),
        ThreadingTCP(("0.0.0.0", 2525), Smtp),
    ]
    tls = ThreadingHTTPServer(("0.0.0.0", 443), Target)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    tls.socket = ctx.wrap_socket(tls.socket, server_side=True)
    servers.append(tls)
    for s in servers:
        s.daemon_threads = True
        threading.Thread(target=s.serve_forever, daemon=True).start()
    print("mock ready", flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
