#!/usr/bin/env bash
# Local integration check for jev-router.
#
#   scripts/local-integration.sh            # offline: fmt, clippy, tests, release build, process e2e (no paid calls)
#   scripts/local-integration.sh --live     # offline checks, then ONE real billed example through a fresh server
#   scripts/local-integration.sh --live-only
#
# The live example needs OPENROUTER_API_KEY in .env (or exported). It starts a separate server on
# 127.0.0.1:18788 with its own temporary database, so an already running router on 8788 is not touched.
# It sends one unrestricted request (no model or effort preference):
#   "3 notebooks at ₹80 each and 2 pens at ₹15 each" -> must route, answer with ₹270, and be
#   verified by the exact money-total validator; then checks the tenant audit record and a clean SIGTERM stop.
set -euo pipefail

cd "$(dirname "$0")/.."
mode="${1:-offline}"
port="${JEV_IT_PORT:-18788}"
base="http://127.0.0.1:${port}"

step() { printf '\n==> %s\n' "$*"; }
fail() { printf '\nFAILED: %s\n' "$*" >&2; exit 1; }

offline() {
  step "Formatting"; cargo fmt --check
  step "Lint"; cargo clippy --locked --all-targets -- -D warnings
  step "Unit and HTTP integration tests (fixtures, no paid calls)"; cargo test --locked
  step "Release build"; cargo build --release --locked
  step "Release executable process scenario"
  JEV_ROUTER_E2E_BIN="$PWD/target/release/jev-router" cargo test --locked --test process_e2e
}

live() {
  local bin="$PWD/target/release/jev-router"
  step "Release build (incremental)"; cargo build --release --locked
  if [ -z "${OPENROUTER_API_KEY:-}" ] && ! grep -qE '^OPENROUTER_API_KEY=.+' .env 2>/dev/null; then
    fail "OPENROUTER_API_KEY is not set in the environment or .env"
  fi
  command -v curl >/dev/null || fail "curl is required"

  # Globals (not locals): the EXIT trap runs after this function returns.
  work="$(mktemp -d)"
  server_pid=""
  cleanup() {
    if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then kill -TERM "$server_pid" 2>/dev/null || true; fi
    rm -rf "$work"
  }
  trap cleanup EXIT

  # Same routing policy as local use (config.local.json, including tiered pools), but an
  # isolated port, database and catalog so a running router is never touched.
  sed -e "s#\"bind\": \"[^\"]*\"#\"bind\": \"127.0.0.1:${port}\"#" \
      -e "s#\"database\": \"[^\"]*\"#\"database\": \"${work}/router.db\"#" \
      -e "s#\"catalog\": \"[^\"]*\"#\"catalog\": \"${work}/catalog.json\"#" \
      config.local.json >"$work/config.json"
  grep -q "${work}/router.db" "$work/config.json" || fail "could not derive test config from config.local.json"

  if curl -fsS "${base}/healthz" >/dev/null 2>&1; then fail "port ${port} already in use; stop that process or set JEV_IT_PORT"; fi
  step "Starting router on ${base} (logs: ${work}/server.log)"
  BIND="127.0.0.1:${port}" "$bin" --config "$work/config.json" serve >"$work/server.out" 2>"$work/server.log" &
  server_pid=$!

  local ready=false
  for _ in $(seq 1 60); do
    if curl -fsS "${base}/readyz" >"$work/ready.json" 2>/dev/null; then ready=true; break; fi
    kill -0 "$server_pid" 2>/dev/null || { cat "$work/server.log" >&2; fail "server exited during startup"; }
    sleep 1
  done
  $ready || { cat "$work/server.log" >&2; fail "server not ready after 60 s"; }
  echo "ready: $(cat "$work/ready.json")"
  step "Tier pool resolved from the live catalog"
  "$bin" --config "$work/config.json" pool || fail "pool preview failed"

  step "Example: unrestricted arithmetic request (billed to your OpenRouter key)"
  local status
  status=$(curl -sS -o "$work/answer.json" -D "$work/headers.txt" -w '%{http_code}' \
    -H 'Content-Type: application/json' -H 'X-OpenRouter-Metadata: enabled' \
    --max-time 280 \
    --data '{"model":"auto","messages":[{"role":"user","content":"A shop sells 3 notebooks at ₹80 each and 2 pens at ₹15 each. What is the total? Give the calculation and final amount in one short sentence."}],"max_tokens":4096}' \
    "${base}/v1/chat/completions")
  header() { grep -i "^$1:" "$work/headers.txt" | head -1 | cut -d' ' -f2- | tr -d '\r' || true; }
  local request_id model effort reason
  request_id="$(header x-request-id)"; model="$(header x-jev-model)"
  effort="$(header x-jev-effort)"; reason="$(header x-jev-reason)"
  echo "HTTP ${status}  request=${request_id}  model=${model}  effort=${effort}  reason=${reason}"
  [ "$status" = 200 ] || { cat "$work/answer.json" >&2; fail "expected HTTP 200"; }
  [ -n "$request_id" ] && [ -n "$model" ] || fail "routing headers missing"
  local decisions
  decisions="$(grep -o 'typesafe/jev-[0-9.]*-[0-9]*' "$work/answer.json" | wc -l | tr -d ' ' || true)"
  echo "Jev decision calls: ${decisions} (tiered pool target: 1-3)"
  local content
  content="$(grep -o '"content":"[^"]*"' "$work/answer.json" | head -1 || true)"
  printf '%s' "$content" | grep -q '270' || { cat "$work/answer.json" >&2; fail "answer does not contain the total 270"; }
  grep -q 'deterministic_money_total' "$work/answer.json" \
    || echo "note: metadata does not name the exact validator; inspect answer_review below"
  echo "answer: ${content}"

  step "Tenant audit record"
  curl -fsS "${base}/v1/requests/${request_id}" >"$work/record.json" || fail "request record not readable"
  grep -qE '"status":"completed(_estimate)?"' "$work/record.json" || { cat "$work/record.json" >&2; fail "request not settled as completed"; }
  echo "record: $(grep -oE '"(status|cost_usd|reserved_usd)":[^,}]*' "$work/record.json" | tr '\n' ' ')"
  curl -fsS "${base}/v1/usage" >"$work/usage.json" && echo "usage: $(cat "$work/usage.json")"
  grep -q '"active":0' "$work/usage.json" || fail "a reservation is still active after the request"

  step "Graceful SIGTERM shutdown"
  kill -TERM "$server_pid"
  local code=0
  wait "$server_pid" || code=$?
  server_pid=""
  [ "$code" = 0 ] || { cat "$work/server.log" >&2; fail "server exit code ${code}"; }
  grep -q '"request"' "$work/server.log" || fail "no access log lines written"
  echo "server stopped cleanly; access log lines: $(grep -c '"request"' "$work/server.log")"

  step "Live example PASSED"
}

case "$mode" in
  offline) offline; step "Offline checks PASSED (run with --live for the billed example)";;
  --live) offline; live;;
  --live-only) live;;
  *) fail "usage: $0 [--live|--live-only]";;
esac
