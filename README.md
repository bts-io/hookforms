<p align="center">
  <img src="logo.png" alt="HookForms" width="340">
</p>

<p align="center">
  <strong>Self-hosted webhook inbox with multi-channel notifications.</strong><br>
  Point your HTML forms at a HookForms endpoint and get submissions delivered to Discord, Slack, email, Teams, Telegram, ntfy, or any webhook URL.
</p>

<p align="center">
  <a href="https://hookforms-docs.h1n054ur.dev">Docs</a> &middot;
  <a href="https://github.com/bts-io/hookforms-cloud">Cloudflare Workers Version</a> &middot;
  <a href="https://hookforms-docs.h1n054ur.dev/getting-started/self-hosted/">Quick Start Guide</a>
</p>

---

```
HTML Form  -->  POST /hooks/your-inbox  -->  Discord, Slack, Email, Telegram, ...
```

> **Want serverless instead?** Check out [hookforms-cloud](https://github.com/bts-io/hookforms-cloud). It runs entirely on Cloudflare Workers (D1, KV, Queues). No Docker or VPS needed.
>
> [![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/bts-io/hookforms-cloud)

## Features

- **Multi-channel notifications** -- route submissions to Discord, Slack, Microsoft Teams, Telegram, ntfy, email, or any webhook URL
- **Auto-detection** -- paste a URL and HookForms detects the channel type automatically (Discord, Slack, Teams, etc.)
- **Multiple email providers** -- Gmail (OAuth), Resend, SendGrid, or SMTP -- configure globally or per-inbox
- **Turnstile bot protection** -- optional Cloudflare Turnstile verification per inbox
- **Security hardened** -- SSRF protection, secret redaction in API responses, brute-force lockout, security headers, request size limits
- **Scoped API keys** -- fine-grained access control with PBKDF2-SHA256 hashed key storage
- **Rate limiting** -- Redis-backed per-IP sliding window with fail-closed behavior
- **Event history** -- stored events with configurable retention (default 30 days)
- **Cloudflare Tunnel** -- expose to the internet without opening ports
- **Docker Compose** -- one command to deploy everything
- **Backward compatible** -- legacy `notify_email` and `forward_url` still work

## Quick Start

```bash
git clone https://github.com/bts-io/hookforms.git
cd hookforms

# Configure
cp .env.example .env
# Edit .env — set POSTGRES_PASSWORD, ADMIN_API_KEY, REDIS_PASSWORD

# Deploy
docker compose up -d

# Run database migrations
docker compose exec api alembic upgrade head
```

Your API is now running at `http://localhost:8000`.

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
# Send to Discord (auto-detected from URL)
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "webhook", "config": {"url": "https://discord.com/api/webhooks/123/abc"}}'

# Send to Slack (auto-detected from URL)
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "webhook", "config": {"url": "https://hooks.slack.com/services/T00/B00/xxx"}}'

# Send email via configured provider
curl -X POST http://localhost:8000/v1/hooks/inboxes/contact-form/channels \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "email", "config": {"recipients": ["team@example.com"]}}'
```

When you pass `type: "webhook"`, HookForms auto-detects the platform from the URL and formats the payload accordingly. Each inbox can have multiple channels -- all fire in parallel.

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

Submissions are delivered to all configured channels with rich formatting (Discord embeds, Slack blocks, HTML emails, etc.).

## Notification Channels

| Channel | Auto-detected from |
|---------|-------------------|
| Discord | `discord.com/api/webhooks/` |
| Slack | `hooks.slack.com/services/` |
| Microsoft Teams | `*.webhook.office.com/` |
| Telegram | `api.telegram.org/bot` |
| ntfy | `ntfy.sh/` |
| Webhook | Any other URL |
| Email | Set `type: "email"` with config |

## Email Providers

Configure a global email provider, or override per-inbox:

```bash
curl -X PUT http://localhost:8000/v1/hooks/config/email-provider \
  -H "X-API-Key: YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"type": "resend", "config": {"api_key": "re_...", "from_email": "noreply@yourdomain.com"}}'
```

Supported: **Gmail** (OAuth), **Resend**, **SendGrid**, **SMTP**.

Provider resolution: inbox-specific > global > env-based Gmail.

## Security

- **SSRF protection** -- blocks private IPs, reserved ranges, Docker service names, and cloud metadata endpoints. Dual-layer: URL validation + connection-time DNS resolution check.
- **Secret redaction** -- channel configs and provider credentials are automatically masked in API read responses (`webhook_url`, `api_key`, `password`, etc.).
- **Brute-force lockout** -- 10 failed auth attempts from the same IP triggers a 5-minute lockout.
- **Key hashing** -- API keys stored as PBKDF2-SHA256 hashes; admin key compared with constant-time `secrets.compare_digest`.
- **Security headers** -- CSP, HSTS, X-Frame-Options, X-Content-Type-Options on all responses.
- **Request size limit** -- 2 MB max body, returns 413 before reading.
- **Rate limiting** -- 100 requests per 60s per IP (fail-closed: returns 503 if Redis unavailable).
- **Email rate limiting** -- 10 emails per 10 minutes per inbox to prevent quota abuse.

## Architecture

```
                         ┌──────────────┐
  HTML Form / curl  ───> │  cloudflared │ ──> ┌─────────┐
                         │  (optional)  │     │   API   │ ──> PostgreSQL
                         └──────────────┘     │ FastAPI │ ──> Redis
                                              └─────────┘
                                                   │
                                              Dispatcher
                                             ┌──┬──┬──┐
                                             │  │  │  │
                                        Discord Slack Email ...

                                              ┌─────────┐
                                              │ Worker  │ (event cleanup cron)
                                              │   ARQ   │
                                              └─────────┘
```

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `POSTGRES_PASSWORD` | Yes | -- | Database password |
| `ADMIN_API_KEY` | Yes | -- | Master admin API key |
| `REDIS_PASSWORD` | Yes | -- | Redis password |
| `GMAIL_SENDER_EMAIL` | No | -- | Gmail sender address (env-based provider) |
| `CORS_ORIGINS` | No | `*` | Comma-separated allowed origins |
| `EVENT_RETENTION_DAYS` | No | `30` | Days to keep events before cleanup |
| `DATABASE_URL` | No | auto | PostgreSQL URL (auto-constructed in Docker) |
| `REDIS_URL` | No | auto | Redis URL (auto-constructed in Docker) |

## Gmail Setup

1. Go to [Google Cloud Console](https://console.cloud.google.com/apis/credentials)
2. Create an OAuth 2.0 Client ID (Desktop application)
3. Download the JSON and save as `config/gmail/credentials.json`
4. Run the auth script:

```bash
pip install google-auth-oauthlib
python scripts/gmail_auth.py
```

5. Set `GMAIL_SENDER_EMAIL` in `.env`
6. Restart: `docker compose restart api worker`

## Cloudflare Tunnel (Optional)

Expose HookForms to the internet without opening ports:

```bash
# Copy and edit config
cp config/cloudflared/config.yml.example config/cloudflared/config.yml
# Place your tunnel credentials JSON in config/cloudflared/

# Start with the tunnel profile
docker compose --profile tunnel up -d
```

## API Reference

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
| `DELETE` | `/v1/hooks/inboxes/{slug}` | Delete inbox + events |
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

## License

[MIT](LICENSE)
