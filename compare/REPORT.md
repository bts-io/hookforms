# HookForms: Python vs Rust comparison

Run: 2026-10-27 17:37:47 UTC. Python from `v0.1.0-python` (docker compose project `hfcompare`), Rust release binary with SQLite in a temp dir. Same ADMIN_API_KEY, same requests, same order.

**247 cases: 237 identical, 10 known and accepted differences, 0 unexpected differences.**

## What is normalised

- UUIDs become `<uuid>`, raw API keys `hf_<43 chars>` (format still checked).
- Timestamps keep their format, digits become `D`, the fraction becomes `.F` (pydantic trims trailing zeros, so its length varies run to run).
- Response headers `date`, `server`, `content-length`, `connection`, `transfer-encoding` are dropped; `allow` is compared as a sorted list (hash-seeded order in Python); `x-ratelimit-reset` digits are masked; `x-ratelimit-remaining` is compared as zero or non-zero; a `location` keeps its path, while its scheme and host (each app's own address) become `<base>`.
- Stored event `host` header and `source_ip` differ by how each app is reached (docker proxy vs direct) and are masked.
- Outbound requests: `host`, `user-agent` (python-httpx vs HookForms/0.2.0), `content-length`, `accept`, `accept-encoding`, `connection` are dropped, and http vs https is ignored (Python reaches Turnstile, Resend and SendGrid over HTTPS through mapped hosts and a test CA, Rust over HTTP through `HOOKFORMS_TEST_UPSTREAM_BASE`). Records are sorted, because both apps send channels concurrently.
- Each case sends its own `X-Forwarded-For`, so the per-IP rate limit and auth lockout do not leak between cases.

## Known, accepted differences

- `duplicate_slug`: Duplicate slug: Python 500 (IntegrityError), Rust 409.
- `negative_limit`: Negative limit: Python 500 from Postgres, Rust 422 validation error.
- `bad_uuid_revoke`: Revoking a non-UUID key id: Python 500, Rust 404.
- `null_is_active`: PATCH is_active null: Python 500 (NOT NULL), Rust ignores the null field.
- `non_object_bodies`: JSON array/primitive bodies: Python stores them but its event schema (body: dict) fails on read (500 on list/get); Rust lists them.
- `bad_uuid_channel`: Channel id that is not a UUID: Python 500, Rust 404 (same class as bad_uuid_revoke).
- `url_key_channels`: Discord/Slack/Teams channels configured with `url`: Python validates but its formatter only reads webhook_url (sends to an empty URL); Rust sends to `url`.
- `health_checks`: Health checks: Python reports postgres and redis, Rust reports database (SQLite, no Redis), by design. The version differs too (see version).
- `version`: Version: `/` and `/health` report 0.1.0 on Python and 0.2.0 on Rust.
- `head_405`: HEAD on /hooks/{slug}: answer parity with Starlette 405 (Allow header order is hash-seeded).

## Unexpected differences seen on many cases

None.

## Results

| # | Case | Result |
|---|------|--------|
| 1 | root | known |
| 2 | health | known |
| 3 | HEAD root | pass |
| 4 | unknown path 404 | pass |
| 5 | POST root 405 | pass |
| 6 | CORS simple request | known |
| 7 | CORS preflight | pass |
| 8 | trailing slash inboxes | pass |
| 9 | no API key 401 | pass |
| 10 | wrong API key 401 | pass |
| 11 | create webhooks key | pass |
| 12 | create admin key | pass |
| 13 | create key default scopes | pass |
| 14 | create key invalid scope 422 | pass |
| 15 | create key missing name 422 | pass |
| 16 | create key name too long 422 | pass |
| 17 | create key name wrong type 422 | pass |
| 18 | create key invalid JSON 422 | pass |
| 19 | create key no body 422 | pass |
| 20 | list keys with webhooks key 403 | pass |
| 21 | list keys with no-scope key 403 | pass |
| 22 | list inboxes with webhooks key | pass |
| 23 | admin route with DB admin key | pass |
| 24 | list keys | pass |
| 25 | list keys page 2 | pass |
| 26 | list keys limit 201 422 | pass |
| 27 | list keys limit not int 422 | pass |
| 28 | list keys negative limit | known |
| 29 | revoke key | pass |
| 30 | revoked key 401 | pass |
| 31 | revoke unknown key 404 | pass |
| 32 | revoke bad uuid | known |
| 33 | PUT keys 405 | pass |
| 34 | auth lockout after 10 failures | pass |
| 35 | create inbox basic | pass |
| 36 | create inbox full | pass |
| 37 | create inbox duplicate slug | known |
| 38 | create inbox missing slug 422 | pass |
| 39 | create inbox slug wrong type 422 | pass |
| 40 | create inbox forward ftp 400 | pass |
| 41 | create inbox forward localhost 400 | pass |
| 42 | create inbox forward private IP 400 | pass |
| 43 | create inbox forward unresolvable 400 | pass |
| 44 | create inbox forward no scheme 400 | pass |
| 45 | list inboxes | pass |
| 46 | list inboxes limit 1 | pass |
| 47 | list inboxes offset past end | pass |
| 48 | list inboxes limit 0 | pass |
| 49 | update inbox description | pass |
| 50 | update inbox empty body | pass |
| 51 | update inbox unknown field ignored | pass |
| 52 | update inbox wrong type 422 | pass |
| 53 | update inbox bad forward 400 | pass |
| 54 | create inbox for null is_active | pass |
| 55 | update inbox null is_active | known |
| 56 | submit after null is_active | known |
| 57 | update unknown inbox 404 | pass |
| 58 | create inbox to deactivate | pass |
| 59 | deactivate inbox | pass |
| 60 | submit to inactive inbox 404 | pass |
| 61 | create inbox to delete | pass |
| 62 | delete inbox | pass |
| 63 | delete unknown inbox 404 | pass |
| 64 | submit to unknown inbox 404 | pass |
| 65 | body JSON object | pass |
| 66 | body JSON object (stored event) | pass |
| 67 | body JSON array | pass |
| 68 | body JSON string | pass |
| 69 | body JSON number | pass |
| 70 | body JSON null | pass |
| 71 | body JSON null (stored event) | pass |
| 72 | body JSON true | pass |
| 73 | body JSON unicode | pass |
| 74 | body JSON unicode (stored event) | pass |
| 75 | body JSON nested | pass |
| 76 | body JSON nested (stored event) | pass |
| 77 | body JSON empty object | pass |
| 78 | body JSON empty object (stored event) | pass |
| 79 | body urlencoded | pass |
| 80 | body urlencoded (stored event) | pass |
| 81 | body urlencoded repeated keys | pass |
| 82 | body urlencoded repeated keys (stored event) | pass |
| 83 | body urlencoded with charset | pass |
| 84 | body urlencoded with charset (stored event) | pass |
| 85 | body urlencoded empty value | pass |
| 86 | body urlencoded empty value (stored event) | pass |
| 87 | body multipart | pass |
| 88 | body multipart (stored event) | pass |
| 89 | body multipart broken | pass |
| 90 | body multipart broken (stored event) | pass |
| 91 | body raw text | pass |
| 92 | body raw text (stored event) | pass |
| 93 | body raw xml | pass |
| 94 | body raw xml (stored event) | pass |
| 95 | body raw no content type | pass |
| 96 | body raw no content type (stored event) | pass |
| 97 | body invalid JSON with JSON type | pass |
| 98 | body invalid JSON with JSON type (stored event) | pass |
| 99 | body invalid UTF-8 | pass |
| 100 | body invalid UTF-8 (stored event) | pass |
| 101 | body empty POST | pass |
| 102 | body empty POST (stored event) | pass |
| 103 | body large (1.5 MB) | pass |
| 104 | body large (1.5 MB) (stored event) | pass |
| 105 | body over 2 MB 413 | pass |
| 106 | GET with query params | pass |
| 107 | GET with query params (stored event) | pass |
| 108 | PUT JSON | pass |
| 109 | PUT JSON (stored event) | pass |
| 110 | PATCH JSON | pass |
| 111 | PATCH JSON (stored event) | pass |
| 112 | DELETE no body | pass |
| 113 | DELETE no body (stored event) | pass |
| 114 | HEAD on hooks | pass |
| 115 | OPTIONS on hooks | pass |
| 116 | headers and CF-Connecting-IP | pass |
| 117 | headers and CF-Connecting-IP (stored event) | pass |
| 118 | list events | pass |
| 119 | list events page | pass |
| 120 | list events limit 500 422 | pass |
| 121 | list events unknown inbox 404 | pass |
| 122 | get event unknown id 404 | pass |
| 123 | get event bad uuid | pass |
| 124 | get event wrong inbox 404 | pass |
| 125 | events with webhooks key | pass |
| 126 | create inbox turnstile | pass |
| 127 | turnstile missing token 400 | pass |
| 128 | turnstile good token | pass |
| 129 | turnstile good token (stored event) | pass |
| 130 | turnstile bad token 403 | pass |
| 131 | turnstile non-JSON verify answer | pass |
| 132 | turnstile form token | pass |
| 133 | turnstile form token (stored event) | pass |
| 134 | turnstile skipped for array body | pass |
| 135 | turnstile skipped for empty body | pass |
| 136 | turnstile skipped for empty body (stored event) | pass |
| 137 | create inbox forward | pass |
| 138 | forward JSON POST | pass |
| 139 | forward JSON POST (stored event) | pass |
| 140 | forward PUT keeps method | pass |
| 141 | forward PUT keeps method (stored event) | pass |
| 142 | forward urlencoded | pass |
| 143 | forward urlencoded (stored event) | pass |
| 144 | create inbox forward discord | pass |
| 145 | forward discord embed | pass |
| 146 | forward discord embed (stored event) | pass |
| 147 | create inbox forward slack | pass |
| 148 | forward slack blocks | pass |
| 149 | forward slack blocks (stored event) | pass |
| 150 | create inbox forward redirect | pass |
| 151 | forward follows redirect | pass |
| 152 | forward follows redirect (stored event) | pass |
| 153 | create inbox forward 500 | pass |
| 154 | forward target 500 still 200 | pass |
| 155 | forward target 500 still 200 (stored event) | pass |
| 156 | create inbox ch-discord | pass |
| 157 | add discord channel | pass |
| 158 | create inbox ch-slack | pass |
| 159 | add slack channel | pass |
| 160 | create inbox ch-teams | pass |
| 161 | add teams channel | pass |
| 162 | create inbox ch-telegram | pass |
| 163 | add telegram channel | pass |
| 164 | create inbox ch-ntfy | pass |
| 165 | add ntfy channel | pass |
| 166 | create inbox ch-webhook | pass |
| 167 | add webhook channel | pass |
| 168 | add webhook channel autodetects discord | pass |
| 169 | add channel typo suggestion 400 | pass |
| 170 | add channel unknown type 400 | pass |
| 171 | add channel missing config 422 | pass |
| 172 | add channel config not object 422 | pass |
| 173 | add discord non-discord URL 400 | pass |
| 174 | add discord via url key | pass |
| 175 | add slack missing URL 400 | pass |
| 176 | add teams ftp URL 400 | pass |
| 177 | add telegram missing chat_id 400 | pass |
| 178 | add ntfy bad priority 400 | pass |
| 179 | add webhook headers not object 400 | pass |
| 180 | add email no recipients 400 | pass |
| 181 | add email bad address 400 | pass |
| 182 | add channel unknown inbox 404 | pass |
| 183 | list channels | pass |
| 184 | list channels unknown inbox 404 | pass |
| 185 | update channel label | pass |
| 186 | update channel invalid type 400 | pass |
| 187 | update unknown channel 404 | pass |
| 188 | update channel bad uuid | known |
| 189 | delete channel bad uuid | known |
| 190 | delete channel | pass |
| 191 | delete unknown channel 404 | pass |
| 192 | dispatch discord | pass |
| 193 | dispatch discord (stored event) | pass |
| 194 | dispatch slack | pass |
| 195 | dispatch slack (stored event) | pass |
| 196 | dispatch teams | pass |
| 197 | dispatch teams (stored event) | pass |
| 198 | dispatch telegram | pass |
| 199 | dispatch telegram (stored event) | pass |
| 200 | dispatch ntfy | pass |
| 201 | dispatch ntfy (stored event) | pass |
| 202 | dispatch webhook | pass |
| 203 | dispatch webhook (stored event) | pass |
| 204 | dispatch discord minimal body | pass |
| 205 | dispatch discord minimal body (stored event) | pass |
| 206 | dispatch ntfy urlencoded | pass |
| 207 | dispatch ntfy urlencoded (stored event) | pass |
| 208 | dispatch webhook raw text | pass |
| 209 | dispatch webhook raw text (stored event) | pass |
| 210 | deactivate channel | pass |
| 211 | dispatch with inactive channel | pass |
| 212 | dispatch with inactive channel (stored event) | pass |
| 213 | get email provider none | pass |
| 214 | set global smtp provider | pass |
| 215 | get global provider | pass |
| 216 | set provider bad type 400 | pass |
| 217 | set provider missing field 400 | pass |
| 218 | set provider unknown inbox 404 | pass |
| 219 | set provider missing config 422 | pass |
| 220 | create inbox em-resend | pass |
| 221 | set resend provider for em-resend | pass |
| 222 | get resend provider | pass |
| 223 | add email channel em-resend | pass |
| 224 | email via resend | pass |
| 225 | email via resend (stored event) | pass |
| 226 | create inbox em-sendgrid | pass |
| 227 | set sendgrid provider for em-sendgrid | pass |
| 228 | get sendgrid provider | pass |
| 229 | add email channel em-sendgrid | pass |
| 230 | email via sendgrid | pass |
| 231 | email via sendgrid (stored event) | pass |
| 232 | create inbox em-smtp | pass |
| 233 | add email channel em-smtp | pass |
| 234 | email via smtp | pass |
| 235 | email via smtp (stored event) | pass |
| 236 | email via smtp no name | pass |
| 237 | email via smtp no name (stored event) | pass |
| 238 | email via smtp unicode | pass |
| 239 | email via smtp unicode (stored event) | pass |
| 240 | email rate limit (12 posts, 10 mails) | pass |
| 241 | delete inbox provider | pass |
| 242 | delete inbox provider again (both 204) | pass |
| 243 | delete provider unknown inbox 404 | pass |
| 244 | email falls back to global smtp | pass |
| 245 | email falls back to global smtp (stored event) | pass |
| 246 | delete global provider | pass |
| 247 | rate limit 429 after 100 | pass |

## Unexpected differences

None.
## Known differences as observed

### 1. root (`version`)

Python: status 200, Rust: status 200

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "name": "HookForms",
      "status": "ok",
      "version": "0.1.0"
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  },
  "rust": {
    "body": {
      "name": "HookForms",
      "status": "ok",
      "version": "0.2.0"
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  }
}
```

</details>

### 2. health (`health_checks`)

Python: status 200, Rust: status 200

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "checks": {
        "postgres": "ok",
        "redis": "ok"
      },
      "status": "healthy",
      "timestamp": "DDDD-DD-DDTDD:DD:DD.F+00:00",
      "version": "0.1.0"
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  },
  "rust": {
    "body": {
      "checks": {
        "database": "ok"
      },
      "status": "healthy",
      "timestamp": "DDDD-DD-DDTDD:DD:DD.F+00:00",
      "version": "0.2.0"
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  }
}
```

</details>

### 6. CORS simple request (`version`)

Python: status 200, Rust: status 200

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "name": "HookForms",
      "status": "ok",
      "version": "0.1.0"
    },
    "headers": {
      "access-control-allow-credentials": "true",
      "access-control-allow-origin": "*",
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  },
  "rust": {
    "body": {
      "name": "HookForms",
      "status": "ok",
      "version": "0.2.0"
    },
    "headers": {
      "access-control-allow-credentials": "true",
      "access-control-allow-origin": "*",
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  }
}
```

</details>

### 28. list keys negative limit (`negative_limit`)

Python: status 500, Rust: status 422

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "error": {
        "code": 422,
        "details": [
          {
            "input": "-1",
            "loc": [
              "query",
              "limit"
            ],
            "msg": "Input should be greater than or equal to 0",
            "type": "greater_than_equal"
          }
        ],
        "message": "Validation error"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 422
  }
}
```

</details>

### 32. revoke bad uuid (`bad_uuid_revoke`)

Python: status 500, Rust: status 404

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "error": {
        "code": 404,
        "message": "API key not found"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 404
  }
}
```

</details>

### 37. create inbox duplicate slug (`duplicate_slug`)

Python: status 500, Rust: status 409

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "error": {
        "code": 409,
        "message": "Inbox slug already exists"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 409
  }
}
```

</details>

### 55. update inbox null is_active (`null_is_active`)

Python: status 500, Rust: status 200

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "data": {
        "created_at": "DDDD-DD-DDTDD:DD:DD.FZ",
        "description": null,
        "email_subject_prefix": null,
        "forward_url": null,
        "has_turnstile": false,
        "id": "<uuid>",
        "is_active": true,
        "notify_email": null,
        "sender_name": null,
        "slug": "nullact"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  }
}
```

</details>

### 56. submit after null is_active (`null_is_active`)

Python: status 404, Rust: status 200

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 404,
        "message": "Inbox not found"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 404
  },
  "rust": {
    "body": {
      "event_id": "<uuid>",
      "status": "received"
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 200
  }
}
```

</details>

### 188. update channel bad uuid (`bad_uuid_channel`)

Python: status 500, Rust: status 404

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "error": {
        "code": 404,
        "message": "Channel not found"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 404
  }
}
```

</details>

### 189. delete channel bad uuid (`bad_uuid_channel`)

Python: status 500, Rust: status 404

<details><summary>Both answers</summary>

```json
{
  "python": {
    "body": {
      "error": {
        "code": 500,
        "message": "Internal server error"
      }
    },
    "headers": {
      "content-type": "application/json"
    },
    "status": 500
  },
  "rust": {
    "body": {
      "error": {
        "code": 404,
        "message": "Channel not found"
      }
    },
    "headers": {
      "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
      "content-type": "application/json",
      "referrer-policy": "strict-origin-when-cross-origin",
      "strict-transport-security": "max-age=31536000; includeSubDomains",
      "x-content-type-options": "nosniff",
      "x-frame-options": "DENY",
      "x-ratelimit-limit": "100",
      "x-ratelimit-remaining": "<n>",
      "x-ratelimit-reset": "DDDDDDDDDD",
      "x-xss-protection": "1; mode=block"
    },
    "status": 404
  }
}
```

</details>

