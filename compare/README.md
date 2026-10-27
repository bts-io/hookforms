# Python vs Rust comparison

This harness sends the same requests to the last Python release and to the Rust version, then diffs the answers: status, headers, JSON bodies, and the outbound calls each app makes to Discord, Slack, Resend and the rest, which all go to a local mock. It writes the result to `REPORT.md`.

The Python version is no longer in this tree. `run.sh` checks it out from the git tag `v0.1.0-python`, the last commit before the switch to Rust, and builds it with that commit's own `docker-compose.yml`. If the tag is not in your clone, the script fetches it from GitHub. `HFCOMPARE_PYTHON_REF` picks another tag or commit.

## Run

Needs Docker, Python 3, OpenSSL and a release build of the Rust binary.

```bash
cargo build --release
compare/run.sh                      # or: compare/run.sh path/to/hookforms
```

Everything it starts (Postgres, Redis, the Python API, the mock, and the Rust binary on a temp SQLite file) is removed when it ends. The admin key and passwords are throwaway values made for the run.

Settings:

| Variable | Default | What it does |
|----------|---------|--------------|
| `HFCOMPARE_PYTHON_REF` | `v0.1.0-python` | Tag or commit with the Python version |
| `HFCOMPARE_PYTHON_REPO` | `https://github.com/bts-io/hookforms.git` | Where to fetch the tag from when it is missing locally |
| `HFCOMPARE_LOCK` | `$XDG_RUNTIME_DIR/hfcompare.lock` | Lock file held while the Python image builds |
| `HFCOMPARE_KEEP_LOGS` | | Folder to copy both apps' logs into |

The cases live in `cases.py` and the mock in `mock/mock.py`. The differences we expect and accept are listed at the top of `REPORT.md`.
