#!/usr/bin/env bash
# Python vs Rust comparison: bring up both apps and a mock, run compare.py,
# write compare/REPORT.md, tear everything down.
#
#   compare/run.sh [path-to-rust-binary]
#
# Default binary: $CARGO_TARGET_DIR (or ../target)/release/hookforms; build it first.
# The Python side is the last Python release, taken from the git tag v0.1.0-python
# (HFCOMPARE_PYTHON_REF picks another tag or commit). When the tag is not in the
# local clone, it is fetched from HFCOMPARE_PYTHON_REPO.
# Secrets are throwaway values generated here and never printed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
RS_DIR="$(dirname "$HERE")"
BIN="${1:-${CARGO_TARGET_DIR:-$RS_DIR/target}/release/hookforms}"
LOCK="${HFCOMPARE_LOCK:-${XDG_RUNTIME_DIR:-/tmp}/hfcompare.lock}"
export HFCOMPARE_PYTHON_REF="${HFCOMPARE_PYTHON_REF:-v0.1.0-python}"
PY_REPO="${HFCOMPARE_PYTHON_REPO:-https://github.com/bts-io/hookforms.git}"
PROJECT=hfcompare
[ -x "$BIN" ] || { echo "Rust binary not found: $BIN" >&2; exit 2; }

WORK="$(mktemp -d)"
SELFHOSTED="$WORK/python"
RS_PID=""
compose() {
  docker compose -p "$PROJECT" -f "$SELFHOSTED/docker-compose.yml" -f "$HERE/compose.override.yml" "$@"
}
cleanup() {
  [ -n "$RS_PID" ] && kill "$RS_PID" 2>/dev/null && wait "$RS_PID" 2>/dev/null || true
  compose down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# Check out the Python version. In this repo's own history the app sits at the root
# (api/, docker-compose.yml); in a clone of the larger repo it sits under selfhosted/.
# git archive limits itself to the current folder, so every call runs from the top.
PY_REF="$HFCOMPARE_PYTHON_REF"
TOP="$(git -C "$RS_DIR" rev-parse --show-toplevel)"
if git -C "$TOP" rev-parse -q --verify "$PY_REF^{commit}" >/dev/null; then
  ref="$PY_REF"
else
  echo "fetching $PY_REF from $PY_REPO"
  git -C "$TOP" fetch -q --no-tags "$PY_REPO" "refs/tags/$PY_REF"
  ref=FETCH_HEAD
fi
sub=""
if git -C "$TOP" cat-file -e "$ref:selfhosted/api/Dockerfile" 2>/dev/null; then sub=selfhosted; fi
git -C "$TOP" cat-file -e "$ref:${sub:+$sub/}api/Dockerfile" 2>/dev/null \
  || { echo "no Python app (api/Dockerfile) in $PY_REF" >&2; exit 2; }
mkdir -p "$SELFHOSTED"
git -C "$TOP" archive --format=tar "$ref${sub:+:$sub}" | tar -x -C "$SELFHOSTED"
echo "Python version: $PY_REF ($(git -C "$TOP" rev-parse --short "$ref^{commit}"))"

export ADMIN_API_KEY="hf_admin_$(openssl rand -hex 24)"
export POSTGRES_PASSWORD="$(openssl rand -hex 16)"
export REDIS_PASSWORD="$(openssl rand -hex 16)"
export HFCOMPARE_CERTS="$WORK/certs" HFCOMPARE_MOCK="$HERE/mock"

# Test CA and a certificate for the hosts Python calls over HTTPS.
mkdir -p "$HFCOMPARE_CERTS"
(
  cd "$HFCOMPARE_CERTS"
  openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 1 -subj "/CN=hfcompare test CA" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -keyout mock.key -out mock.csr -subj "/CN=mock" 2>/dev/null
  printf 'subjectAltName=DNS:challenges.cloudflare.com,DNS:api.resend.com,DNS:api.sendgrid.com\n' > ext.cnf
  openssl x509 -req -in mock.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out mock.pem -days 1 -extfile ext.cnf 2>/dev/null
  chmod 644 ./*.pem ./*.key
)

echo "building the Python image (shared lock)"
flock "$LOCK" nice -n 19 docker compose -p "$PROJECT" -f "$SELFHOSTED/docker-compose.yml" \
  -f "$HERE/compose.override.yml" build -q api
compose up -d postgres redis mock api >/dev/null
for _ in $(seq 60); do
  compose exec -T api alembic upgrade head >/dev/null 2>&1 && break
  sleep 1
done
compose exec -T api alembic current 2>/dev/null | grep -q head || { echo "Python migrations failed" >&2; exit 1; }

echo "starting the Rust binary"
(
  cd "$WORK"
  exec env -i PATH="$PATH" HOME="$WORK" ADMIN_API_KEY="$ADMIN_API_KEY" \
    DATABASE_URL="sqlite://$WORK/hookforms.db" \
    HOOKFORMS_TEST_UPSTREAM_BASE="http://198.18.0.10:8080" RUST_LOG=warn \
    "$BIN" >"$WORK/rust.log" 2>&1
) &
RS_PID=$!

for url in http://127.0.0.1:18800/health http://127.0.0.1:8000/health http://198.18.0.10:8081/_records; do
  for _ in $(seq 60); do curl -sf -o /dev/null "$url" && break; sleep 0.5; done
  curl -sf -o /dev/null "$url" || { echo "not up: $url" >&2; exit 1; }
done

rc=0
python3 "$HERE/compare.py" || rc=$?
if [ -n "${HFCOMPARE_KEEP_LOGS:-}" ]; then
  cp "$WORK/rust.log" "$HFCOMPARE_KEEP_LOGS/rust.log" || true
  compose logs api >"$HFCOMPARE_KEEP_LOGS/python.log" 2>&1 || true
fi
exit "$rc"
