# Contributing to HookForms

Thanks for your interest in contributing. Here's how to get started.

## Development setup

You need a current stable Rust toolchain (install it with [rustup](https://rustup.rs)). Docker is only needed to build the image or run the comparison harness.

```bash
git clone https://github.com/bts-io/hookforms.git
cd hookforms
cp .env.example .env
# Set ADMIN_API_KEY in .env

DATABASE_URL=sqlite://./hookforms.db cargo run
curl http://127.0.0.1:8000/health
```

The database file is created and migrated on start. `RUST_LOG=debug` shows more logs.

## Making changes

1. Fork the repo and create a branch from `main`.
2. Make your changes, with tests for new behaviour.
3. Run the same checks as CI:

   ```bash
   cargo fmt --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --locked
   ```

4. If you changed the Dockerfile or startup, check the image too: `docker compose up -d --build` (with `build: .` in place of the `image:` line).
5. Open a pull request.

The Postgres import test needs a Postgres server and is skipped by default:

```bash
docker run --rm -d --name hookforms-test-pg -e POSTGRES_PASSWORD=test -p 127.0.0.1:55432:5432 postgres:16
HOOKFORMS_TEST_PG_URL=postgres://postgres:test@127.0.0.1:55432/postgres \
  cargo test --test import_postgres -- --ignored
docker stop hookforms-test-pg
```

## Code layout

- `src/routes/`: one file or folder per group of HTTP routes
- `src/channels/`: Discord, Slack, Teams, Telegram, ntfy and webhook senders
- `src/email/`: Gmail, Resend, SendGrid and SMTP providers
- `migrations/`: SQLite schema, applied on start
- `compare/`: harness that checks answers against the last Python release (see its README)

## Code style

- `cargo fmt` formatting, no clippy warnings
- The HTTP API should keep answering as it does now: request and response fields, status codes and error shapes. If a change has to alter one, say so in the pull request.

## Reporting issues

Use GitHub Issues. Include:

- Steps to reproduce
- Expected and actual behaviour
- Logs if relevant (`docker compose logs api`)
