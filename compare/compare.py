"""Send every case in cases.py to the Python and the Rust app and diff the answers.

Usage (run.sh sets these): ADMIN_API_KEY, PY_URL, RS_URL, MOCK_CTL, RS_SRC.
Writes REPORT.md next to this file. Exit code 1 when an unexpected difference exists.
"""

import http.client
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from cases import CASES  # noqa: E402

ADMIN = os.environ["ADMIN_API_KEY"]
PY = os.environ.get("PY_URL", "http://127.0.0.1:18800")
RS = os.environ.get("RS_URL", "http://127.0.0.1:8000")
MOCK_CTL = os.environ.get("MOCK_CTL", "http://198.18.0.10:8081")
RS_SRC = os.environ.get("RS_SRC", "198.18.0.1")

# Accepted differences: Python answered 500 (or behaved worse) and Rust answers
# properly. A case marked with one of these passes when Python shows the old
# behaviour; the report still prints both answers.
KNOWN = {
    "duplicate_slug": "Duplicate slug: Python 500 (IntegrityError), Rust 409.",
    "negative_limit": "Negative limit: Python 500 from Postgres, Rust 422 validation error.",
    "bad_uuid_revoke": "Revoking a non-UUID key id: Python 500, Rust 404.",
    "null_is_active": "PATCH is_active null: Python 500 (NOT NULL), Rust ignores the null field.",
    "non_object_bodies": "JSON array/primitive bodies: Python stores them but its event schema "
                         "(body: dict) fails on read (500 on list/get); Rust lists them.",
    "bad_uuid_channel": "Channel id that is not a UUID: Python 500, Rust 404 (same class as bad_uuid_revoke).",
    "url_key_channels": "Discord/Slack/Teams channels configured with `url`: Python validates but "
                        "its formatter only reads webhook_url (sends to an empty URL); Rust sends to `url`.",
    "health_checks": "Health checks: Python reports postgres and redis, Rust reports database (SQLite, no Redis), "
                     "by design. The version differs too (see version).",
    "version": "Version: `/` and `/health` report 0.1.0 on Python and 0.2.0 on Rust.",
    "head_405": "HEAD on /hooks/{slug}: answer parity with Starlette 405 (Allow header order is hash-seeded).",
}

UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
UUID_ANY = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
DT_RE = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?")
KEY_RE = re.compile(r"^hf_[A-Za-z0-9_-]{43}$")
DROP_RESP_HEADERS = {"date", "server", "content-length", "connection", "transfer-encoding", "keep-alive"}
DROP_OUT_HEADERS = {"host", "user-agent", "content-length", "accept", "accept-encoding", "connection"}


def mask_dt(m):
    s, frac, tz = m.group(0), m.group(1) or "", m.group(2) or ""
    base = s[: len(s) - len(frac) - len(tz)]
    return re.sub(r"\d", "D", base) + (".F" if frac else "") + tz


def norm_str(s, key=None):
    if key in ("host",):
        return "<host>"
    if key in ("source_ip", "remoteip") and s:
        return "<ip>"
    if UUID_RE.match(s):
        return "<uuid>"
    if KEY_RE.match(s):
        return "hf_<43 chars>"
    s = DT_RE.sub(mask_dt, s)
    s = UUID_ANY.sub("<uuid>", s)
    return s


def norm(v, key=None):
    if isinstance(v, dict):
        return {k: norm(x, k.lower() if isinstance(k, str) else k) for k, x in v.items()}
    if isinstance(v, list):
        return [norm(x) for x in v]
    if isinstance(v, str):
        return norm_str(v, key)
    return v


def pick(obj, path):
    for part in path.split("."):
        if not isinstance(obj, dict):
            return None
        obj = obj.get(part)
    return obj


class Side:
    def __init__(self, name, base):
        self.name, self.base, self.vars = name, urllib.parse.urlsplit(base), {}

    def send(self, method, path, body=None, headers=None):
        conn = http.client.HTTPConnection(self.base.hostname, self.base.port, timeout=30)
        try:
            conn.request(method, path, body=body, headers=headers or {})
        except OSError:
            pass  # the server may answer (413) and close before reading the whole body
        try:
            r = conn.getresponse()
        except (OSError, http.client.HTTPException) as exc:
            conn.close()
            return 0, {}, f"connection error: {type(exc).__name__}".encode()
        data = r.read()
        hdrs = {}
        for k, v in r.getheaders():
            hdrs.setdefault(k.lower(), []).append(v)
        conn.close()
        return r.status, hdrs, data


def mock_records():
    with urllib.request.urlopen(MOCK_CTL + "/_records", timeout=10) as r:
        return json.loads(r.read())


def mock_reset():
    urllib.request.urlopen(urllib.request.Request(MOCK_CTL + "/_reset", method="POST"), timeout=10).read()


def wait_outbound(max_wait=8.0, quiet=1.0):
    t0 = time.time()
    last, last_change = -1, time.time()
    while time.time() - t0 < max_wait:
        n = len(mock_records())
        if n != last:
            last, last_change = n, time.time()
        elif time.time() - last_change >= quiet:
            break
        time.sleep(0.2)
    return mock_records()


def norm_headers(h):
    out = {}
    for k, vals in h.items():
        if k in DROP_RESP_HEADERS:
            continue
        if k == "allow":
            vals = [", ".join(sorted(x.strip() for v in vals for x in v.split(",")))]
        if k == "x-ratelimit-reset":
            vals = [re.sub(r"\d", "D", v) for v in vals]
        if k == "x-ratelimit-remaining":
            vals = ["<n>" if v != "0" else "0" for v in vals]
        if k == "location":
            # Absolute redirects name each app's own address; keep only the path.
            vals = [re.sub(r"^https?://[^/]+", "<base>", v) for v in vals]
        out[k] = vals if len(vals) > 1 else vals[0]
    return dict(sorted(out.items()))


def norm_body(data, show=True):
    if not data:
        return ""
    try:
        return norm(json.loads(data))
    except ValueError:
        text = data.decode("utf-8", errors="replace")
        return norm_str(text) if show else f"<{len(data)} bytes>"


def norm_record(r):
    r = dict(r)
    r.pop("src", None)
    r.pop("proto", None)
    if "headers" in r:
        r["headers"] = {k: v for k, v in r["headers"].items() if k not in DROP_OUT_HEADERS}
    return norm(r)


def split_records(recs):
    py = sorted((norm_record(r) for r in recs if r["src"] != RS_SRC), key=lambda x: json.dumps(x, sort_keys=True))
    rs = sorted((norm_record(r) for r in recs if r["src"] == RS_SRC), key=lambda x: json.dumps(x, sort_keys=True))
    return py, rs


SIDES = [Side("python", PY), Side("rust", RS)]
CORS_NO_ORIGIN = ("access-control-allow-credentials", "access-control-allow-origin", "vary")
GLOBAL_DIFFS = {}
XFF_N = [0]


def next_ip():
    XFF_N[0] += 1
    n = XFF_N[0]
    return f"10.{(n >> 16) & 255}.{(n >> 8) & 255}.{n & 255}"


def build(side, c):
    def sub(s):
        return re.sub(r"\{(\w+)\}", lambda m: str(side.vars.get(m.group(1), "<missing>")), s)

    headers = {k: sub(v) for k, v in c.get("headers", {}).items()}
    key = c.get("key", "admin")
    if key == "admin":
        headers["X-API-Key"] = ADMIN
    elif key == "bad":
        headers["X-API-Key"] = "hf_wrong_key_value"
    elif key.startswith("var:"):
        headers["X-API-Key"] = side.vars.get(key[4:], "missing")
    body = c.get("raw")
    if "json" in c:
        body = json.dumps(c["json"]).encode()
        headers.setdefault("Content-Type", "application/json")
    if isinstance(body, str):
        body = body.encode()
    return sub(c["path"]), body, headers


def run_plain(c, xff):
    answers = {}
    for side in SIDES:
        path, body, headers = build(side, c)
        headers.setdefault("X-Forwarded-For", xff)
        status, hdrs, data = side.send(c["method"], path, body, headers)
        try:
            j = json.loads(data) if data else None
        except ValueError:
            j = None
        for var, p in (c.get("save") or {}).items():
            val = pick(j, p) if isinstance(j, dict) else None
            if val is not None:
                side.vars[var] = val
        answers[side.name] = {"status": status, "headers": norm_headers(hdrs),
                              "body": norm_body(data, c.get("show_body", True))}
    return answers


def run_special(c, xff):
    kind = c["special"]
    answers = {}
    if kind == "rate_limit":
        for side in SIDES:
            ip = next_ip()
            for _ in range(100):
                side.send("GET", "/", None, {"X-Forwarded-For": ip})
            s, h, d = side.send("GET", "/", None, {"X-Forwarded-For": ip})
            answers[side.name] = {"status": s, "headers": norm_headers(h), "body": norm_body(d)}
    elif kind == "lockout":
        for side in SIDES:
            ip = next_ip()
            codes = [side.send("GET", "/v1/hooks/inboxes", None, {"X-Forwarded-For": ip, "X-API-Key": "hf_bad"})[0]
                     for _ in range(10)]
            s, h, d = side.send("GET", "/v1/hooks/inboxes", None, {"X-Forwarded-For": ip, "X-API-Key": ADMIN})
            answers[side.name] = {"first_10_statuses": codes, "status": s, "headers": norm_headers(h), "body": norm_body(d)}
    elif kind == "email_rate":
        mock_reset()
        for side in SIDES:
            hdr = {"X-API-Key": ADMIN, "Content-Type": "application/json", "X-Forwarded-For": xff}
            side.send("POST", "/v1/hooks/inboxes", json.dumps({"slug": "em-rate"}).encode(), hdr)
            side.send("POST", "/v1/hooks/inboxes/em-rate/channels",
                      json.dumps({"type": "email", "config": {"recipients": ["r@example.com"]}}).encode(), hdr)
            codes = []
            for i in range(12):
                codes.append(side.send("POST", "/hooks/em-rate", json.dumps({"name": f"N{i}"}).encode(),
                                       {"Content-Type": "application/json", "X-Forwarded-For": xff})[0])
            answers[side.name] = {"statuses": codes}
        recs = wait_outbound(max_wait=20, quiet=2.0)
        py, rs = split_records(recs)
        answers["python"]["mails"] = len(py)
        answers["rust"]["mails"] = len(rs)
        answers["python"]["subjects"] = sorted(r.get("subject", "") for r in py)
        answers["rust"]["subjects"] = sorted(r.get("subject", "") for r in rs)
    return answers


def main():
    results = []
    limit = int(os.environ.get("HFCOMPARE_LIMIT") or 0) or len(CASES)
    for i, c in enumerate(CASES[:limit], 1):
        xff = next_ip()
        missing = c.get("needs") and any(c["needs"] not in s.vars for s in SIDES)
        if c.get("special"):
            answers, outbound = run_special(c, xff), None
        else:
            mock_reset()
            answers = run_plain(c, xff)
            recs = wait_outbound() if c.get("out") else mock_records()
            py, rs = split_records(recs)
            outbound = {"python": py, "rust": rs} if (py or rs) else None
            if outbound:
                answers["python"]["outbound"] = py
                answers["rust"]["outbound"] = rs
        if "Origin" not in c.get("headers", {}):
            got = {}
            for side in ("python", "rust"):
                h = answers[side].get("headers", {})
                got[side] = {k: h.pop(k) for k in CORS_NO_ORIGIN if k in h}
            if got["python"] != got["rust"]:
                GLOBAL_DIFFS.setdefault("cors_headers_without_origin", []).append((i, got))
        same = answers["python"] == answers["rust"]
        known = c.get("known")
        if same:
            verdict = "pass"
        elif known:
            verdict = "known"
        else:
            verdict = "FAIL"
        if missing:
            verdict += " (needed value missing on one side)"
        results.append((i, c, verdict, answers))
        print(f"{i:3d} {verdict:6s} {c['name']}", flush=True)
    write_report(results)
    return 1 if GLOBAL_DIFFS or any(v.startswith("FAIL") for _, _, v, _ in results) else 0


def dump(x):
    s = json.dumps(x, indent=2, ensure_ascii=False, sort_keys=True)
    return s if len(s) < 12000 else s[:12000] + "\n... (cut)"


def diff_keys(a, b, prefix=""):
    """Paths where two JSON values differ, for a short summary line."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for k in sorted(set(a) | set(b), key=str):
            out += diff_keys(a.get(k, "<absent>"), b.get(k, "<absent>"), f"{prefix}.{k}" if prefix else str(k))
        return out
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        out = []
        for n, (x, y) in enumerate(zip(a, b)):
            out += diff_keys(x, y, f"{prefix}[{n}]")
        return out
    return [prefix or "(root)"] if a != b else []


def write_report(results):
    n = len(results)
    passed = sum(v.startswith("pass") for _, _, v, _ in results)
    known = sum(v.startswith("known") for _, _, v, _ in results)
    failed = [r for r in results if r[2].startswith("FAIL")]
    lines = [
        "# HookForms: Python vs Rust comparison",
        "",
        f"Run: {time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())} UTC.Python from `{os.environ.get('HFCOMPARE_PYTHON_REF', 'v0.1.0-python')}` "
        "(docker compose project `hfcompare`), "
        "Rust release binary with SQLite in a temp dir. Same ADMIN_API_KEY, same requests, same order.",
        "",
        f"**{n} cases: {passed} identical, {known} known and accepted differences, {len(failed)} unexpected differences.**",
        "",
        "## What is normalised",
        "",
        "- UUIDs become `<uuid>`, raw API keys `hf_<43 chars>` (format still checked).",
        "- Timestamps keep their format, digits become `D`, the fraction becomes `.F` (pydantic trims trailing zeros, so its length varies run to run).",
        "- Response headers `date`, `server`, `content-length`, `connection`, `transfer-encoding` are dropped; "
        "`allow` is compared as a sorted list (hash-seeded order in Python); `x-ratelimit-reset` digits are masked; "
        "`x-ratelimit-remaining` is compared as zero or non-zero; a `location` keeps its path, while its scheme and "
        "host (each app's own address) become `<base>`.",
        "- Stored event `host` header and `source_ip` differ by how each app is reached (docker proxy vs direct) and are masked.",
        "- Outbound requests: `host`, `user-agent` (python-httpx vs HookForms/0.2.0), `content-length`, `accept`, "
        "`accept-encoding`, `connection` are dropped, and http vs https is ignored (Python reaches Turnstile, Resend and "
        "SendGrid over HTTPS through mapped hosts and a test CA, Rust over HTTP through `HOOKFORMS_TEST_UPSTREAM_BASE`). "
        "Records are sorted, because both apps send channels concurrently.",
        "- Each case sends its own `X-Forwarded-For`, so the per-IP rate limit and auth lockout do not leak between cases.",
        "",
        "## Known, accepted differences",
        "",
    ]
    for k, v in KNOWN.items():
        lines.append(f"- `{k}`: {v}")
    lines += ["", "## Unexpected differences seen on many cases", ""]
    if not GLOBAL_DIFFS:
        lines.append("None.")
    for k, hits in GLOBAL_DIFFS.items():
        first = hits[0][1]
        lines += [f"- `{k}`: on {len(hits)} of the requests sent without an `Origin` header the CORS headers differ "
                  "(these headers are taken out of the per-case comparison so other differences stay visible). "
                  f"Example (case {hits[0][0]}): Python `{json.dumps(first['python'])}`, Rust `{json.dumps(first['rust'])}`."]
    lines += ["", "## Results", "", "| # | Case | Result |", "|---|------|--------|"]
    for i, c, v, _ in results:
        lines.append(f"| {i} | {c['name']} | {v} |")
    lines += ["", "## Unexpected differences", ""]
    if not failed:
        lines.append("None.")
    for i, c, v, a in failed:
        req = f"{c['method']} {c['path']}" if not c.get("special") else f"special: {c['special']}"
        lines += [f"### {i}. {c['name']}", "", f"Request: `{req}`", "",
                  "Differs at: " + ", ".join(f"`{p}`" for p in diff_keys(a["python"], a["rust"])[:20]), "",
                  "<details><summary>Python</summary>", "", "```json", dump(a["python"]), "```", "", "</details>", "",
                  "<details><summary>Rust</summary>", "", "```json", dump(a["rust"]), "```", "", "</details>", ""]
    lines += ["## Known differences as observed", ""]
    for i, c, v, a in results:
        if v.startswith("known"):
            lines += [f"### {i}. {c['name']} (`{c['known']}`)", "",
                      f"Python: status {a['python'].get('status')}, Rust: status {a['rust'].get('status')}", "",
                      "<details><summary>Both answers</summary>", "", "```json",
                      dump({"python": a["python"], "rust": a["rust"]}), "```", "", "</details>", ""]
    Path(__file__).with_name("REPORT.md").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    sys.exit(main())
