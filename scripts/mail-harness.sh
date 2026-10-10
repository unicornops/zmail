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

# The harness images live on Docker Hub, whose shared anonymous pull quota
# exhausts on busy GitHub runner pools (issue #88). Optional DOCKERHUB_USERNAME
# and DOCKERHUB_TOKEN environment variables switch the pulls to a personal
# quota, and the pre-pull retries below keep a transient registry hiccup from
# failing the run.
pull_images() {
  if [ -n "${DOCKERHUB_USERNAME:-}" ] && [ -n "${DOCKERHUB_TOKEN:-}" ]; then
    echo "${DOCKERHUB_TOKEN}" | docker login --username "${DOCKERHUB_USERNAME}" --password-stdin
  fi
  local images=(
    "greenmail/standalone:2.1.8"
    "apache/james:memory-3.9.0"
    "nginx:1.27-alpine"
  )
  local image attempt
  for image in "${images[@]}"; do
    for attempt in 1 2 3; do
      if docker pull "$image" >/dev/null 2>&1; then
        break
      fi
      echo "pull attempt ${attempt} for ${image} failed" >&2
      sleep $((attempt * 20))
      if [ "$attempt" -eq 3 ]; then
        echo "could not pull ${image}" >&2
        return 1
      fi
    done
  done
}

case "${1:-up}" in
  up)
    ensure_james_keystore
    pull_images
    run_compose up -d --remove-orphans
    provision_jmap_accounts
    ;;
  down)
    run_compose down -v --remove-orphans
    ;;
  reset)
    ensure_james_keystore
    pull_images
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
