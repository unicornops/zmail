#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COMPOSE_FILE="${REPO_ROOT}/docker/mail-harness/docker-compose.yml"
PROJECT_NAME="zmail-phase2b-mail"
KEYSTORE_DIR="${REPO_ROOT}/docker/mail-harness/james/conf"
KEYSTORE_PATH="${KEYSTORE_DIR}/keystore"
JMAP_ADMIN_URL="http://127.0.0.1:4081"

wait_for_jmap_admin() {
  for _ in $(seq 1 60); do
    if curl -fsS "${JMAP_ADMIN_URL}/domains" >/dev/null 2>&1; then
      return
    fi
    sleep 1
  done

  echo "James WebAdmin did not become ready in time" >&2
  exit 1
}

provision_jmap_accounts() {
  wait_for_jmap_admin
  curl -fsS -X PUT "${JMAP_ADMIN_URL}/domains/localhost" >/dev/null
  curl -fsS -X PUT \
    -H "Content-Type: application/json" \
    --data '{"password":"jmap-password"}' \
    "${JMAP_ADMIN_URL}/users/jmap@localhost" >/dev/null
  curl -fsS -X PUT \
    -H "Content-Type: application/json" \
    --data '{"password":"sender-password"}' \
    "${JMAP_ADMIN_URL}/users/sender@localhost" >/dev/null
}

ensure_james_keystore() {
  if [[ -f "${KEYSTORE_PATH}" ]]; then
    return
  fi

  mkdir -p "${KEYSTORE_DIR}"
  keytool -genkeypair \
    -alias james \
    -keyalg RSA \
    -keysize 2048 \
    -storetype PKCS12 \
    -keystore "${KEYSTORE_PATH}" \
    -storepass james72laBalle \
    -keypass james72laBalle \
    -dname "CN=localhost, OU=Zmail, O=Zmail, L=Test, ST=Test, C=US" \
    -validity 3650 \
    -noprompt
}

run_compose() {
  docker compose -p "${PROJECT_NAME}" -f "${COMPOSE_FILE}" "$@"
}

case "${1:-up}" in
  up)
    ensure_james_keystore
    run_compose up -d --remove-orphans
    provision_jmap_accounts
    ;;
  down)
    run_compose down -v --remove-orphans
    ;;
  reset)
    ensure_james_keystore
    run_compose down -v --remove-orphans
    run_compose up -d --remove-orphans
    provision_jmap_accounts
    ;;
  logs)
    run_compose logs -f
    ;;
  ps)
    run_compose ps
    ;;
  *)
    echo "usage: scripts/mail-harness.sh [up|down|reset|logs|ps]" >&2
    exit 1
    ;;
esac
