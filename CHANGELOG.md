# Changelog

## 0.2.0

HookForms is rewritten in Rust. It now runs as one container with a SQLite database, in place of the API, worker, Postgres and Redis containers of 0.1. The HTTP API stays the same.

### Changed

- One binary and one container. The image is about 20.6 MB (0.1: 1.39 GB of images for the API, Postgres and Redis) and the service uses about 7.4 MiB of memory at idle (0.1: about 188 MiB across its four containers).
- Data lives in a SQLite file (`/data/hookforms.db` in the `hookforms-data` volume). The schema is created and migrated on start, so there is no separate migration step.
- The nightly event cleanup (03:00 UTC, `EVENT_RETENTION_DAYS`) runs inside the service. The worker container is gone.
- Rate limit and auth lockout counters are kept in memory and reset on restart. With no Redis, the limiter no longer answers 503.
- A ready-made image is published for `linux/amd64` and `linux/arm64` as `ghcr.io/bts-io/hookforms`, and `docker-compose.yml` uses it by default. `build: .` still builds it locally.
- `POSTGRES_PASSWORD`, `REDIS_PASSWORD`, `REDIS_URL` and a Postgres `DATABASE_URL` from 0.1 are ignored with a log line, so an old `.env` works unchanged.

### Added

- `hookforms import-postgres` copies a 0.1 Postgres database into SQLite (inboxes, events, API keys, channels and email providers) and checks the row counts on both sides. Existing API keys keep working. The README has the upgrade steps.

### Fixed

Cases where 0.1 answered 500 now get a proper answer:

- A duplicate inbox slug returns 409.
- A negative `limit` returns a 422 validation error.
- A key or channel id that is not a UUID returns 404.
- `"is_active": null` in a `PATCH` leaves the field unchanged.
- Events with a JSON array or plain value as body can be listed and read.
- Discord, Slack and Teams channels configured with `url` instead of `webhook_url` now send to that URL.

### Other differences

- `/health` reports the SQLite database instead of Postgres and Redis.
- Outgoing requests use the user agent `HookForms/0.2.0`.

The Python version stays available at the git tag `v0.1.0-python`.

## 0.1.0

The Python version (FastAPI, Postgres, Redis, ARQ worker).
