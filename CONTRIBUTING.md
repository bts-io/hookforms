# Contributing to HookForms

Thanks for your interest in contributing! Here's how to get started.

## Development Setup

```bash
git clone https://github.com/bts-io/hookforms.git
cd hookforms
cp .env.example .env
# Edit .env with your own values

docker compose up -d
docker compose exec api alembic upgrade head
```

## Making Changes

1. Fork the repo and create a branch from `main`
2. Make your changes
3. Run the linter: `ruff check api/ && ruff format --check api/`
4. Test your changes locally with `docker compose up -d`
5. Open a pull request

## Code Style

- Python 3.12+
- Ruff for linting and formatting (line length 100)
- Type hints everywhere
- Async by default

## Reporting Issues

Use GitHub Issues. Include:
- Steps to reproduce
- Expected vs actual behavior
- Docker logs if relevant (`docker compose logs api`)
