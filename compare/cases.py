"""Test cases for the Python vs Rust comparison.

Each case is one request sent to both apps in the same order. A case may save
values from its response (per app) for later paths and bodies, written as
{name}. `out=True` waits for outbound requests (mock HTTP/SMTP) and compares
them too. `known` names an accepted difference from KNOWN in compare.py.

Special cases (`special=...`) run a small scripted sequence instead.
"""

MOCK = "http://198.18.0.10:8080"
BIG = "x" * (1536 * 1024)
TOO_BIG = "y" * (2 * 1024 * 1024 + 10)
UUID0 = "00000000-0000-4000-8000-000000000000"

CASES = []


def case(name, method, path, **kw):
    CASES.append(dict(name=name, method=method, path=path, **kw))


def hook(name, slug, body=None, ctype=None, method="POST", query="", out=False, **kw):
    """Submit to /hooks/{slug} and then read the stored event back."""
    headers = dict(kw.pop("headers", {}))
    if ctype:
        headers["Content-Type"] = ctype
    case(name, method, f"/hooks/{slug}{query}", raw=body, headers=headers, key="none",
         save={"last_event": "event_id"}, out=out, **kw)
    if kw.get("known") is None and not kw.get("no_readback"):
        case(name + " (stored event)", "GET", f"/v1/hooks/{slug}/events/{{last_event}}",
             needs="last_event")


def jhook(name, slug, obj, **kw):
    import json as _j
    hook(name, slug, _j.dumps(obj).encode(), "application/json", **kw)


# --- root, health, routing -------------------------------------------------
case("root", "GET", "/", key="none", known="version")
case("health", "GET", "/health", key="none", known="health_checks")
case("HEAD root", "HEAD", "/", key="none")
case("unknown path 404", "GET", "/nope", key="none")
case("POST root 405", "POST", "/", key="none")
case("CORS simple request", "GET", "/", key="none", headers={"Origin": "https://site.example"}, known="version")
case("CORS preflight", "OPTIONS", "/v1/hooks/inboxes", key="none",
     headers={"Origin": "https://site.example", "Access-Control-Request-Method": "POST",
              "Access-Control-Request-Headers": "x-api-key,content-type"})
case("trailing slash inboxes", "GET", "/v1/hooks/inboxes/", key="admin")

# --- auth and keys ---------------------------------------------------------
case("no API key 401", "GET", "/v1/hooks/inboxes", key="none")
case("wrong API key 401", "GET", "/v1/hooks/inboxes", key="bad")
case("create webhooks key", "POST", "/v1/auth/keys", json={"name": "wh", "scopes": ["webhooks"]},
     save={"wh_key": "data.raw_key", "wh_id": "data.id"})
case("create admin key", "POST", "/v1/auth/keys", json={"name": "adm", "scopes": ["admin"]},
     save={"adm_key": "data.raw_key"})
case("create key default scopes", "POST", "/v1/auth/keys", json={"name": "noscope"},
     save={"ns_key": "data.raw_key", "ns_id": "data.id"})
case("create key invalid scope 422", "POST", "/v1/auth/keys", json={"name": "x", "scopes": ["bogus", "admin", "zz"]})
case("create key missing name 422", "POST", "/v1/auth/keys", json={})
case("create key name too long 422", "POST", "/v1/auth/keys", json={"name": "n" * 101})
case("create key name wrong type 422", "POST", "/v1/auth/keys", json={"name": 5})
case("create key invalid JSON 422", "POST", "/v1/auth/keys", raw=b'{"name": ', headers={"Content-Type": "application/json"})
case("create key no body 422", "POST", "/v1/auth/keys")
case("list keys with webhooks key 403", "GET", "/v1/auth/keys", key="var:wh_key", needs="wh_key")
case("list keys with no-scope key 403", "GET", "/v1/hooks/inboxes", key="var:ns_key", needs="ns_key")
case("list inboxes with webhooks key", "GET", "/v1/hooks/inboxes", key="var:wh_key", needs="wh_key")
case("admin route with DB admin key", "GET", "/v1/auth/keys?limit=1", key="var:adm_key", needs="adm_key")
case("list keys", "GET", "/v1/auth/keys")
case("list keys page 2", "GET", "/v1/auth/keys?limit=1&offset=1")
case("list keys limit 201 422", "GET", "/v1/auth/keys?limit=201")
case("list keys limit not int 422", "GET", "/v1/auth/keys?limit=abc")
case("list keys negative limit", "GET", "/v1/auth/keys?limit=-1", known="negative_limit")
case("revoke key", "DELETE", "/v1/auth/keys/{ns_id}", needs="ns_id")
case("revoked key 401", "GET", "/v1/hooks/inboxes", key="var:ns_key", needs="ns_key")
case("revoke unknown key 404", "DELETE", f"/v1/auth/keys/{UUID0}")
case("revoke bad uuid", "DELETE", "/v1/auth/keys/not-a-uuid", known="bad_uuid_revoke")
case("PUT keys 405", "PUT", "/v1/auth/keys")
case("auth lockout after 10 failures", "SPECIAL", "", special="lockout")

# --- inboxes ---------------------------------------------------------------
case("create inbox basic", "POST", "/v1/hooks/inboxes", json={"slug": "basic"})
case("create inbox full", "POST", "/v1/hooks/inboxes", json={
    "slug": "full", "description": "Contact form", "notify_email": "a@example.com, b@example.com",
    "email_subject_prefix": "[Site]", "sender_name": "Acme", "turnstile_secret": "sekret"})
case("create inbox duplicate slug", "POST", "/v1/hooks/inboxes", json={"slug": "basic"}, known="duplicate_slug")
case("create inbox missing slug 422", "POST", "/v1/hooks/inboxes", json={"description": "x"})
case("create inbox slug wrong type 422", "POST", "/v1/hooks/inboxes", json={"slug": ["a"]})
case("create inbox forward ftp 400", "POST", "/v1/hooks/inboxes", json={"slug": "f1", "forward_url": "ftp://x.example/"})
case("create inbox forward localhost 400", "POST", "/v1/hooks/inboxes", json={"slug": "f2", "forward_url": "http://localhost/x"})
case("create inbox forward private IP 400", "POST", "/v1/hooks/inboxes", json={"slug": "f3", "forward_url": "http://10.1.2.3/"})
case("create inbox forward unresolvable 400", "POST", "/v1/hooks/inboxes", json={"slug": "f4", "forward_url": "http://no-such-host.invalid/"})
case("create inbox forward no scheme 400", "POST", "/v1/hooks/inboxes", json={"slug": "f5", "forward_url": "example.com/x"})
case("list inboxes", "GET", "/v1/hooks/inboxes")
case("list inboxes limit 1", "GET", "/v1/hooks/inboxes?limit=1&offset=0")
case("list inboxes offset past end", "GET", "/v1/hooks/inboxes?offset=50")
case("list inboxes limit 0", "GET", "/v1/hooks/inboxes?limit=0")
case("update inbox description", "PATCH", "/v1/hooks/inboxes/basic", json={"description": "updated"})
case("update inbox empty body", "PATCH", "/v1/hooks/inboxes/basic", json={})
case("update inbox unknown field ignored", "PATCH", "/v1/hooks/inboxes/basic", json={"zzz": 1})
case("update inbox wrong type 422", "PATCH", "/v1/hooks/inboxes/basic", json={"is_active": "maybe"})
case("update inbox bad forward 400", "PATCH", "/v1/hooks/inboxes/basic", json={"forward_url": "http://127.0.0.1/"})
case("create inbox for null is_active", "POST", "/v1/hooks/inboxes", json={"slug": "nullact"})
case("update inbox null is_active", "PATCH", "/v1/hooks/inboxes/nullact", json={"is_active": None}, known="null_is_active")
case("submit after null is_active", "POST", "/hooks/nullact", json={"a": 1}, key="none", known="null_is_active")
case("update unknown inbox 404", "PATCH", "/v1/hooks/inboxes/nope", json={"description": "x"})
case("create inbox to deactivate", "POST", "/v1/hooks/inboxes", json={"slug": "sleepy"})
case("deactivate inbox", "PATCH", "/v1/hooks/inboxes/sleepy", json={"is_active": False})
case("submit to inactive inbox 404", "POST", "/hooks/sleepy", json={"a": 1}, key="none")
case("create inbox to delete", "POST", "/v1/hooks/inboxes", json={"slug": "gone"})
case("delete inbox", "DELETE", "/v1/hooks/inboxes/gone")
case("delete unknown inbox 404", "DELETE", "/v1/hooks/inboxes/gone")
case("submit to unknown inbox 404", "POST", "/hooks/gone", json={"a": 1}, key="none")

# --- intake body types on "basic" (no channels, no forward) ----------------
jhook("body JSON object", "basic", {"name": "Alice", "email": "a@example.com", "msg": "hi"})
jhook("body JSON array", "basic", [1, 2, {"a": "b"}], known="non_object_bodies")
jhook("body JSON string", "basic", "just a string", known="non_object_bodies")
jhook("body JSON number", "basic", 42.5, known="non_object_bodies")
jhook("body JSON null", "basic", None)
jhook("body JSON true", "basic", True, known="non_object_bodies")
jhook("body JSON unicode", "basic", {"name": "Zoë 日本 🚀", "note": "a\u0000b \"q\" \\"})
jhook("body JSON nested", "basic", {"a": {"b": [1, 2.0, None, True]}, "n": 1e100, "i": 12345678901234567890})
jhook("body JSON empty object", "basic", {})
hook("body urlencoded", "basic", b"name=Bob&email=bob%40example.com&msg=a+b%26c", "application/x-www-form-urlencoded")
hook("body urlencoded repeated keys", "basic", b"tag=a&tag=b&tag=c&x=1", "application/x-www-form-urlencoded")
hook("body urlencoded with charset", "basic", b"name=%C3%A9t%C3%A9", "application/x-www-form-urlencoded; charset=utf-8")
hook("body urlencoded empty value", "basic", b"a=&b=2", "application/x-www-form-urlencoded")
MP = (b"--BOUND\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nCarol\r\n"
      b"--BOUND\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n"
      b"Content-Type: text/plain\r\n\r\nfile body\r\n"
      b"--BOUND\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nline1\nline2\r\n--BOUND--\r\n")
hook("body multipart", "basic", MP, "multipart/form-data; boundary=BOUND")
hook("body multipart broken", "basic", b"--BOUND\r\nbroken", "multipart/form-data; boundary=BOUND")
hook("body raw text", "basic", b"hello plain text", "text/plain")
hook("body raw xml", "basic", b"<a><b>1</b></a>", "application/xml")
hook("body raw no content type", "basic", b"no type here")
hook("body invalid JSON with JSON type", "basic", b"{not json", "application/json")
hook("body invalid UTF-8", "basic", b"\xff\xfe bad", "text/plain")
hook("body empty POST", "basic", b"")
hook("body large (1.5 MB)", "basic", ('{"big":"%s"}' % BIG).encode(), "application/json", show_body=False)
hook("body over 2 MB 413", "basic", TOO_BIG.encode(), "text/plain", show_body=False, no_readback=True)
hook("GET with query params", "basic", None, method="GET", query="?a=1&b=two&a=3")
hook("PUT JSON", "basic", b'{"k":"put"}', "application/json", method="PUT")
hook("PATCH JSON", "basic", b'{"k":"patch"}', "application/json", method="PATCH")
hook("DELETE no body", "basic", None, method="DELETE")
case("HEAD on hooks", "HEAD", "/hooks/basic", key="none", known="head_405")
case("OPTIONS on hooks", "OPTIONS", "/hooks/basic", key="none")
hook("headers and CF-Connecting-IP", "basic", b'{"h":1}', "application/json",
     headers={"CF-Connecting-IP": "192.0.2.7", "X-Custom": "Value", "User-Agent": "cmp/1"})

# --- events ----------------------------------------------------------------
case("list events", "GET", "/v1/hooks/basic/events?limit=5", show_body=False)
case("list events page", "GET", "/v1/hooks/basic/events?limit=2&offset=3")
case("list events limit 500 422", "GET", "/v1/hooks/basic/events?limit=500")
case("list events unknown inbox 404", "GET", "/v1/hooks/nope/events")
case("get event unknown id 404", "GET", f"/v1/hooks/basic/events/{UUID0}")
case("get event bad uuid", "GET", "/v1/hooks/basic/events/not-a-uuid")
case("get event wrong inbox 404", "GET", "/v1/hooks/full/events/{last_event}", needs="last_event")
case("events with webhooks key", "GET", "/v1/hooks/basic/events?limit=1", key="var:wh_key", needs="wh_key")

# --- Turnstile -------------------------------------------------------------
case("create inbox turnstile", "POST", "/v1/hooks/inboxes", json={"slug": "ts", "turnstile_secret": "ts-secret"})
jhook("turnstile missing token 400", "ts", {"name": "x"}, no_readback=True)
jhook("turnstile good token", "ts", {"name": "x", "cf-turnstile-response": "good-token"}, out=True,
      headers={"CF-Connecting-IP": "192.0.2.55"})
jhook("turnstile bad token 403", "ts", {"name": "x", "cf-turnstile-response": "bad-token"}, out=True, no_readback=True)
jhook("turnstile non-JSON verify answer", "ts", {"cf-turnstile-response": "notjson-token"}, out=True, no_readback=True)
hook("turnstile form token", "ts", b"name=y&cf-turnstile-response=good-token", "application/x-www-form-urlencoded", out=True)
jhook("turnstile skipped for array body", "ts", ["a"], known="non_object_bodies")
hook("turnstile skipped for empty body", "ts", b"")

# --- legacy forward_url ----------------------------------------------------
case("create inbox forward", "POST", "/v1/hooks/inboxes", json={"slug": "fwd", "forward_url": f"{MOCK}/forward"})
jhook("forward JSON POST", "fwd", {"name": "F", "n": 1}, out=True)
hook("forward PUT keeps method", "fwd", b'{"k":"v"}', "application/json", method="PUT", out=True)
hook("forward urlencoded", "fwd", b"a=1&b=2", "application/x-www-form-urlencoded", out=True)
case("create inbox forward discord", "POST", "/v1/hooks/inboxes", json={
    "slug": "fwd-discord", "forward_url": f"{MOCK}/discord.com/api/webhooks/1/abc", "email_subject_prefix": "[D]"})
jhook("forward discord embed", "fwd-discord", {"full_name": "Dee", "msg": "x" * 60, "empty": "", "cf-turnstile-response": "t"}, out=True)
case("create inbox forward slack", "POST", "/v1/hooks/inboxes", json={
    "slug": "fwd-slack", "forward_url": f"{MOCK}/hooks.slack.com/services/T/B/X"})
jhook("forward slack blocks", "fwd-slack", {"first_name": "Sam", "list": [1, 2], "obj": {"k": "v"}}, out=True)
case("create inbox forward redirect", "POST", "/v1/hooks/inboxes", json={"slug": "fwd-redir", "forward_url": f"{MOCK}/redirect"})
jhook("forward follows redirect", "fwd-redir", {"r": 1}, out=True)
case("create inbox forward 500", "POST", "/v1/hooks/inboxes", json={"slug": "fwd-500", "forward_url": f"{MOCK}/status500"})
jhook("forward target 500 still 200", "fwd-500", {"r": 1}, out=True)

# --- channels CRUD ---------------------------------------------------------
CH = {
    "discord": {"webhook_url": f"{MOCK}/discord.com/api/webhooks/9/tok"},
    "slack": {"webhook_url": f"{MOCK}/hooks.slack.com/services/A/B/C"},
    "teams": {"webhook_url": f"{MOCK}/teams/webhook.office.com/x"},
    "telegram": {"bot_url": f"{MOCK}/api.telegram.org/bot123:abc", "chat_id": "-1001"},
    "ntfy": {"url": f"{MOCK}/ntfy.sh/topic", "priority": 4},
    "webhook": {"url": f"{MOCK}/generic", "custom_headers": {"X-Token": "abc", "X-Two": "2"}},
}
for t, cfg in CH.items():
    case(f"create inbox ch-{t}", "POST", "/v1/hooks/inboxes",
         json={"slug": f"ch-{t}", "email_subject_prefix": "[CH]", "sender_name": "Acme <Forms>"})
    case(f"add {t} channel", "POST", f"/v1/hooks/inboxes/ch-{t}/channels",
         json={"type": t, "label": f"{t} label", "config": cfg}, save={f"ch_{t}": "data.id"})
case("add webhook channel autodetects discord", "POST", "/v1/hooks/inboxes/ch-webhook/channels",
     json={"type": "webhook", "config": {"url": f"{MOCK}/discord.com/api/webhooks/7/auto"}}, save={"ch_auto": "data.id"})
case("add channel typo suggestion 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "discrod", "config": {}})
case("add channel unknown type 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "pager", "config": {}})
case("add channel missing config 422", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "discord"})
case("add channel config not object 422", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "discord", "config": "x"})
case("add discord non-discord URL 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "discord", "config": {"webhook_url": "https://example.com/x"}})
case("add discord via url key", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "discord", "config": {"url": f"{MOCK}/discord.com/api/webhooks/1/u"}}, known="url_key_channels")
case("add slack missing URL 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "slack", "config": {}})
case("add teams ftp URL 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "teams", "config": {"webhook_url": "ftp://x/y"}})
case("add telegram missing chat_id 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "telegram", "config": {"bot_url": "https://api.telegram.org/bot1:a"}})
case("add ntfy bad priority 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "ntfy", "config": {"url": "https://ntfy.sh/t", "priority": 9}})
case("add webhook headers not object 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "webhook", "config": {"url": "https://example.com/h", "custom_headers": ["a"]}})
case("add email no recipients 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "email", "config": {"recipients": []}})
case("add email bad address 400", "POST", "/v1/hooks/inboxes/basic/channels", json={"type": "email", "config": {"recipients": ["nope"]}})
case("add channel unknown inbox 404", "POST", "/v1/hooks/inboxes/nope/channels", json={"type": "discord", "config": {}})
case("list channels", "GET", "/v1/hooks/inboxes/ch-webhook/channels")
case("list channels unknown inbox 404", "GET", "/v1/hooks/inboxes/nope/channels")
case("update channel label", "PATCH", "/v1/hooks/inboxes/ch-webhook/channels/{ch_auto}", json={"label": "renamed"}, needs="ch_auto")
case("update channel invalid type 400", "PATCH", "/v1/hooks/inboxes/ch-webhook/channels/{ch_auto}", json={"type": "zzz"}, needs="ch_auto")
case("update unknown channel 404", "PATCH", f"/v1/hooks/inboxes/ch-webhook/channels/{UUID0}", json={"label": "x"})
case("update channel bad uuid", "PATCH", "/v1/hooks/inboxes/ch-webhook/channels/not-a-uuid", json={"label": "x"}, known="bad_uuid_channel")
case("delete channel bad uuid", "DELETE", "/v1/hooks/inboxes/ch-webhook/channels/not-a-uuid", known="bad_uuid_channel")
case("delete channel", "DELETE", "/v1/hooks/inboxes/ch-webhook/channels/{ch_auto}", needs="ch_auto")
case("delete unknown channel 404", "DELETE", f"/v1/hooks/inboxes/ch-webhook/channels/{UUID0}")

# --- channel dispatch --------------------------------------------------------
FORM = {"name": "Alice <b>", "email": "alice@example.com", "first_name": "A", "message": "Line 1\nLine 2 & more",
        "tags": ["x", "y"], "meta": {"k": "v", "n": 2}, "ok": True, "count": 3, "price": 9.5, "empty": "",
        "long": "L" * 1100, "cf-turnstile-response": "t", "source": "site", "raw": "r"}
for t in CH:
    jhook(f"dispatch {t}", f"ch-{t}", FORM, out=True)
jhook("dispatch discord minimal body", "ch-discord", {"x": "1"}, out=True)
hook("dispatch ntfy urlencoded", "ch-ntfy", b"name=N&msg=hello", "application/x-www-form-urlencoded", out=True)
hook("dispatch webhook raw text", "ch-webhook", b"raw text body", "text/plain", out=True)
case("deactivate channel", "PATCH", "/v1/hooks/inboxes/ch-slack/channels/{ch_slack}", json={"is_active": False}, needs="ch_slack")
jhook("dispatch with inactive channel", "ch-slack", {"a": "b"}, out=True)

# --- email providers ---------------------------------------------------------
case("get email provider none", "GET", "/v1/hooks/config/email-provider")
SMTP = {"host": "198.18.0.10", "port": 2525, "username": "", "password": "", "use_tls": False, "from_email": "forms@example.com"}
case("set global smtp provider", "PUT", "/v1/hooks/config/email-provider", json={"type": "smtp", "config": SMTP})
case("get global provider", "GET", "/v1/hooks/config/email-provider")
case("set provider bad type 400", "PUT", "/v1/hooks/config/email-provider", json={"type": "pigeon", "config": {}})
case("set provider missing field 400", "PUT", "/v1/hooks/config/email-provider", json={"type": "resend", "config": {"api_key": "k"}})
case("set provider unknown inbox 404", "PUT", "/v1/hooks/config/email-provider", json={"inbox": "nope", "type": "smtp", "config": SMTP})
case("set provider missing config 422", "PUT", "/v1/hooks/config/email-provider", json={"type": "smtp"})
for name, ptype, cfg in [
    ("em-resend", "resend", {"api_key": "re_test_123456789", "from_email": "r@example.com"}),
    ("em-sendgrid", "sendgrid", {"api_key": "SG.test.123456789", "from_email": "s@example.com"}),
]:
    case(f"create inbox {name}", "POST", "/v1/hooks/inboxes", json={"slug": name, "sender_name": "Shop"})
    case(f"set {ptype} provider for {name}", "PUT", "/v1/hooks/config/email-provider", json={"inbox": name, "type": ptype, "config": cfg})
    case(f"get {ptype} provider", "GET", f"/v1/hooks/config/email-provider?inbox={name}")
    case(f"add email channel {name}", "POST", f"/v1/hooks/inboxes/{name}/channels", json={"type": "email", "config": {"recipients": ["one@example.com", "two@example.com"]}})
    jhook(f"email via {ptype}", name, FORM, out=True)
case("create inbox em-smtp", "POST", "/v1/hooks/inboxes", json={"slug": "em-smtp", "email_subject_prefix": "[Contact]"})
case("add email channel em-smtp", "POST", "/v1/hooks/inboxes/em-smtp/channels", json={"type": "email", "config": {"recipients": ["to@example.com"]}})
jhook("email via smtp", "em-smtp", FORM, out=True)
jhook("email via smtp no name", "em-smtp", {"message": "<script>x</script>", "email": "e@example.com"}, out=True)
jhook("email via smtp unicode", "em-smtp", {"name": "Zoë 日本", "note": "héllo"}, out=True)
case("email rate limit (12 posts, 10 mails)", "SPECIAL", "", special="email_rate")
case("delete inbox provider", "DELETE", "/v1/hooks/config/email-provider?inbox=em-resend")
case("delete inbox provider again (both 204)", "DELETE", "/v1/hooks/config/email-provider?inbox=em-resend")
case("delete provider unknown inbox 404", "DELETE", "/v1/hooks/config/email-provider?inbox=nope")
jhook("email falls back to global smtp", "em-resend", {"name": "Fallback"}, out=True)
case("delete global provider", "DELETE", "/v1/hooks/config/email-provider")

# --- request limits ----------------------------------------------------------
case("rate limit 429 after 100", "SPECIAL", "", special="rate_limit")
