<p align="center">
  <img src="logo.png" alt="HookForms" width="340">
</p>

<p align="center">
  <strong>Self-hosted webhook inbox with multi-channel notifications.</strong><br>
  Point your HTML forms at a HookForms endpoint and get submissions delivered to Discord, Slack, email, Teams, Telegram, ntfy, or any webhook URL.
</p>

<p align="center">
  <a href="https://hookforms-docs.h1n054ur.dev">Docs</a> &middot;
  <a href="https://github.com/bts-io/hookforms-cloud">Cloudflare Workers version</a> &middot;
  <a href="https://hookforms-docs.h1n054ur.dev/getting-started/self-hosted/">Quick start guide</a>
</p>

---

```
HTML Form  -->  POST /hooks/your-inbox  -->  Discord, Slack, Email, Telegram, ...
```

> **Want serverless instead?** [hookforms-cloud](https://github.com/bts-io/hookforms-cloud) runs entirely on Cloudflare Workers (D1, KV, Queues), with no Docker or VPS.
>
> [![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/bts-io/hookforms-cloud)

## What's new in 0.2.0

HookForms is now a single Rust binary in one container, with its data in a SQLite file. Postgres, Redis and the separate worker container are gone. The HTTP API is the same, and a comparison run that sends 247 requests to both versions gets the same answer for all but a handful of edge cases, listed under [Differences from the Python version](#differences-from-the-python-version).

| | 0.1 (Python) | 0.2 (Rust) |
|---|---|---|
| Containers | 4 (API, worker, Postgres, Redis) | 1 |
| Images to download | 1.39 GB (API 908 MB, Postgres 420 MB, Redis 59 MB) | 20.6 MB |
| Memory at idle | about 188 MiB across the four containers | about 7.4 MiB |
| Database | Postgres | SQLite file in a volume |

Coming from 0.1? See [Upgrading from the Python version](#upgrading-from-the-python-version). The Python code stays available at the git tag `v0.1.0-python`.

## Features

- Multi-channel notifications: route submissions to Discord, Slack, Microsoft Teams, Telegram, ntfy, email, or any webhook URL
- Auto-detection: paste a URL and HookForms works out the channel type (Discord, Slack, Teams and so on)
- Several email providers: Gmail (OAuth), Resend, SendGrid or SMTP, set globally or per inbox
- Turnstile bot protection: optional Cloudflare Turnstile check per inbox
- Security: SSRF protection, secrets masked in API responses, brute-force lockout, security headers, request size limits
- Scoped API keys: keys are stored as PBKDF2-SHA256 hashes
- Rate limiting: per-IP sliding window, kept in memory
- Event history: stored events with a retention period you choose (30 days by default)
- Cloudflare Tunnel: reach it from the internet without opening ports
- Backward compatible: the older `notify_email` and `forward_url` fields still work

## Quick start

Needs Docker with the Compose plugin. The image is published for `linux/amd64` and `linux/arm64`.

```bash
git clone https://github.com/bts-io/hookforms.git
cd hookforms

cp .env.example .env
# Edit .env and set ADMIN_API_KEY to a long random value

docker compose up -d
curl http://127.0.0.1:8000/health
```

The API now listens on `http://127.0.0.1:8000`. The database is created and migrated on the first start, and lives in the `hookforms-data` volume (`/data/hookforms.db` in the container).

To update later:

```bash
git pull
docker compose pull
docker compose up -d
```

### Building the image yourself

`docker-compose.yml` pulls `ghcr.io/bts-io/hookforms:latest`. To build from this folder instead, replace the `image:` line with `build: .`, then:

```bash
docker compose up -d --build
```

### Running without Docker

Needs a current stable Rust toolchain.

```bash
cp .env.example .env        # set ADMIN_API_KEY
DATABASE_URL=sqlite://./hookforms.db cargo run --release
```

## Usage

### 1. Create an inbox

```bash
curl -X POST http://localhost:8000/v1/hooks/inboxes \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "slug": "contact-form",
    "description": "Website contact form"
  }'
```

### 2. Add notification channels

```bash
# Send to Discord (detected from the URL)
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "webhook", "config": {"url": "https://discord.com/api/webhooks/123/abc"}}'

# Send to Slack (detected from the URL)
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "webhook", "config": {"url": "https://hooks.slack.com/services/T00/B00/xxx"}}'

# Send email through the configured provider
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "email", "config": {"recipients": ["team@example.com"]}}'
```

With `type: "webhook"`, HookForms detects the platform from the URL and formats the payload for it. An inbox can have several channels, and they all fire in parallel.

### 3. Point your form at it

```html
<form action="https://hookforms.yourdomain.com/hooks/contact-form" method="POST">
  <input type="text" name="name" placeholder="Name" required>
  <input type="email" name="email" placeholder="Email" required>
  <textarea name="message" placeholder="Message" required></textarea>
  <button type="submit">Send</button>
</form>
```

### 4. Get notified

Each submission goes to every channel on the inbox, formatted for that platform (Discord embeds, Slack blocks, HTML email and so on).

## Notification channels

| Channel | Detected from |
|---------|---------------|
| Discord | `discord.com/api/webhooks/` |
| Slack | `hooks.slack.com/services/` |
| Microsoft Teams | `*.webhook.office.com/` |
| Telegram | `api.telegram.org/bot` |
| ntfy | `ntfy.sh/` |
| Webhook | Any other URL |
| Email | Set `type: "email"` with a config |

## Email providers

Set a global email provider, or one per inbox:

```bash
curl -X PUT http://localhost:8000/v1/hooks/config/email-provider \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "resend", "config": {"api_key": "re_...", "from_email": "noreply@yourdomain.com"}}'
```

Supported: Gmail (OAuth), Resend, SendGrid and SMTP. We suggest Resend: it needs only an API key and a verified sending domain.

Which provider is used: the inbox's own, then the global one, then the Gmail provider from the environment.

## Configuration

Settings come from `.env` (Docker Compose passes the ones below to the container) or the process environment.

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ADMIN_API_KEY` | Yes | | Master admin API key. The service does not start without it. |
| `CORS_ORIGINS` | No | all origins | Comma-separated allowed origins |
| `EVENT_RETENTION_DAYS` | No | `30` | Days to keep events before the nightly cleanup deletes them |
| `GMAIL_SENDER_EMAIL` | No | | Sender address for the Gmail provider from the environment |
| `GMAIL_CREDENTIALS_PATH` | No | `/app/config/gmail/credentials.json` | Google OAuth client file |
| `GMAIL_TOKEN_PATH` | No | `/app/config/gmail/token.json` | Google OAuth token file |
| `DATABASE_URL` | No | `sqlite:///data/hookforms.db` | SQLite database file. Only `sqlite:` URLs are accepted. |
| `DEBUG` | No | `false` | More detailed logs |
| `RUST_LOG` | No | `info` | Log filter, for example `debug` or `hookforms=debug` |

`POSTGRES_PASSWORD`, `REDIS_PASSWORD`, `REDIS_URL` and a Postgres `DATABASE_URL` left over from 0.1 are ignored, with one log line each at start, so an old `.env` works as it is.

## Event retention

Every day at 03:00 UTC the service deletes events older than `EVENT_RETENTION_DAYS` days and logs how many it removed. This runs inside the same process, so there is no worker container.

## Upgrading from the Python version

0.2.0 keeps its data in SQLite. The `import-postgres` command copies everything from the 0.1 Postgres database into it: inboxes, events, API keys, notification channels and email providers. API keys keep working, because their hashes are copied as they are.

The import reads Postgres in one snapshot and writes SQLite in one transaction, then counts every table on both sides and fails if any count differs. It prints table names and row counts only. It refuses to write into a database that already holds data, unless you add `--force`, which replaces that data.

```
hookforms import-postgres --from postgres://user:pass@host:5432/db [--to /data/hookforms.db] [--force]
```

`--from` also takes the 0.1 `DATABASE_URL` as it is (`postgresql+asyncpg://...`).

These steps assume the 0.1 stack runs from a clone of this repository with the default compose file. Run them from that folder.

1. Stop the old API and worker, but keep Postgres running so the import can read it:

   ```bash
   docker compose stop api worker
   ```

2. Get the new version. `.env` stays as it is.

   ```bash
   git pull
   docker compose pull
   ```

3. Run the import. `docker compose run` joins the same network as the old Postgres container, which is still running:

   ```bash
   set -a; . ./.env; set +a
   docker compose run --rm --no-deps api hookforms import-postgres \
     --from "postgres://hookforms:${POSTGRES_PASSWORD}@postgres:5432/hookforms"
   ```

   It ends with the row count of each table.

4. Start the new version. `--remove-orphans` stops and removes the old worker, Postgres and Redis containers:

   ```bash
   docker compose up -d --remove-orphans
   curl http://127.0.0.1:8000/health
   curl -H "X-API-Key: $ADMIN_API_KEY" http://127.0.0.1:8000/v1/hooks/inboxes
   ```

The old `pgdata` and `redisdata` volumes are kept as a backup. Once everything looks right, remove them (`docker volume ls` shows their exact names, prefixed with the project name):

```bash
docker volume rm hookforms_pgdata hookforms_redisdata
```

If something goes wrong before step 4, `git checkout v0.1.0-python` and `docker compose up -d` bring the old version back on its untouched data.

How values are converted: UUIDs become lowercase text, timestamps UTC text with six fractional digits, JSON columns and the `scopes` array compact JSON text. An `is_active` that is NULL in Postgres becomes false, which is how 0.1 already treated it.

## Differences from the Python version

The API is meant to answer as 0.1 did. These differences are on purpose, mostly where 0.1 answered with a 500:

- Creating an inbox with a slug that already exists returns 409 (0.1: 500).
- A negative `limit` on list endpoints returns a 422 validation error (0.1: 500).
- Revoking an API key or addressing a channel by an id that is not a UUID returns 404 (0.1: 500).
- `PATCH` with `"is_active": null` leaves the field unchanged (0.1: 500).
- Events whose body is a JSON array or a plain value are listed and returned normally (0.1 stored them but answered 500 when reading them).
- Discord, Slack and Teams channels configured with `url` instead of `webhook_url` send to that URL (0.1 accepted the config but sent to an empty URL).
- `/health` reports the SQLite database instead of Postgres and Redis.
- Rate limit and auth lockout counters are kept in memory, so they reset when the service restarts. With Redis gone there is no store that can be down, so the limiter no longer answers 503.
- Outgoing requests use the user agent `HookForms/<version>`.

## Gmail setup

1. In the [Google Cloud Console](https://console.cloud.google.com/apis/credentials), create an OAuth 2.0 Client ID (Desktop application).
2. Download the JSON and save it as `config/gmail/credentials.json`.
3. Run the auth script on your own computer (it opens a browser):

   ```bash
   pip install google-auth-oauthlib
   python scripts/gmail_auth.py
   ```

4. Set `GMAIL_SENDER_EMAIL` in `.env`.
5. Restart: `docker compose restart api`

## Cloudflare Tunnel (optional)

Reach HookForms from the internet without opening ports:

```bash
cp config/cloudflared/config.yml.example config/cloudflared/config.yml
# put the tunnel credentials JSON in config/cloudflared/
docker compose --profile tunnel up -d
```

## API reference

Full documentation at [hookforms-docs.h1n054ur.dev](https://hookforms-docs.h1n054ur.dev).

### Public (no auth)

| Method | Path | Description |
|--------|------|-------------|
| `ANY` | `/hooks/{slug}` | Receive a webhook event |
| `GET` | `/health` | Health check |

### Authenticated (`X-API-Key` header)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/hooks/inboxes` | Create inbox |
| `GET` | `/v1/hooks/inboxes` | List inboxes |
| `PATCH` | `/v1/hooks/inboxes/{slug}` | Update inbox |
| `DELETE` | `/v1/hooks/inboxes/{slug}` | Delete inbox and its events |
| `GET` | `/v1/hooks/{slug}/events` | List events |
| `POST` | `/v1/hooks/inboxes/{slug}/channels` | Add notification channel |
| `GET` | `/v1/hooks/inboxes/{slug}/channels` | List channels |
| `PATCH` | `/v1/hooks/inboxes/{slug}/channels/{id}` | Update channel |
| `DELETE` | `/v1/hooks/inboxes/{slug}/channels/{id}` | Remove channel |
| `PUT` | `/v1/hooks/config/email-provider` | Set email provider |
| `GET` | `/v1/hooks/config/email-provider` | Get email provider config |
| `DELETE` | `/v1/hooks/config/email-provider` | Remove email provider |
| `POST` | `/v1/auth/keys` | Create API key (admin) |
| `GET` | `/v1/auth/keys` | List API keys (admin) |
| `DELETE` | `/v1/auth/keys/{id}` | Revoke API key (admin) |

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md). The `compare/` folder holds the harness that checks this version against the last Python release.

## License

[MIT](LICENSE)
