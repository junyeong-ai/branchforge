#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.backends.local.yml"

export BRANCHFORGE_TEST_POSTGRES_URL="${BRANCHFORGE_TEST_POSTGRES_URL:-postgres://branchforge:branchforge@127.0.0.1:55432/branchforge_test}"
export BRANCHFORGE_TEST_REDIS_URL="${BRANCHFORGE_TEST_REDIS_URL:-redis://127.0.0.1:56379/}"

usage() {
  cat <<'EOF'
Usage: scripts/validate-local-backends.sh [up|test|down|all]

  up    Start local PostgreSQL and Redis test backends
  test  Run ignored backend integration tests against local backends
  down  Stop local PostgreSQL and Redis test backends
  all   Start backends, run tests, then stop backends
EOF
}

up() {
  docker compose -f "$COMPOSE_FILE" up -d --wait
}

test_backends() {
  cargo test --test backend_integration_tests --all-features -- --ignored
}

down() {
  docker compose -f "$COMPOSE_FILE" down -v
}

main() {
  local command="${1:-all}"
  case "$command" in
    up)
      up
      ;;
    test)
      test_backends
      ;;
    down)
      down
      ;;
    all)
      up
      test_backends
      down
      ;;
    *)
      usage
      exit 1
      ;;
  esac
}

main "$@"
