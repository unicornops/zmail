#!/usr/bin/env bash
set -euo pipefail

# proton-harness.sh - Proton API test double for the live Proton tests.
#
# Usage: scripts/proton-harness.sh [up|seed|down|reset|build|env|logs|ps]
#
#   build  build the server image (or, in native mode, the binaries) only
#   up     build the image, start a fresh server, wait until it answers and
#          seed the test account (a stopped or stale harness is replaced)
#   seed   seed the test account into a running, unseeded harness
#   down   stop the server and remove its state
#   reset  down, then up
#   env    print the harness settings as shell exports
#   logs   print the server log
#   ps     show whether the server is running
#
# The server is go-proton-api's fake server (docker/proton-harness), serving
# the Proton API over TLS on https://127.0.0.1:18443 with a certificate from
# a throwaway CA generated on every `up`, and its gRPC control service on
# 127.0.0.1:18080. `up` writes everything a test needs to
# $PROTON_HARNESS_STATE_DIR (default target/proton-harness):
#
#   ca.pem       the CA to trust; nothing else is signed by it
#   state.json   base URL, CA path, account, addresses and labels
#   env          the same as PROTON_HARNESS_* shell exports
#
# PROTON_HARNESS_MODE=native builds and runs the server with a local Go
# toolchain instead of Docker (same pin, same patch), for machines without
# a working Docker.
#
# Environment:
#   PROTON_HARNESS_MODE           docker (default) or native
#   PROTON_HARNESS_STATE_DIR      where state is written
#   PROTON_HARNESS_READY_TIMEOUT  seconds to wait for readiness (default 120)
#   DOCKERHUB_USERNAME/TOKEN      optional Docker Hub login for the base
#                                 image pull (issue #88)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
HARNESS_DIR="${REPO_ROOT}/docker/proton-harness"

GO_PROTON_API_REPO="https://github.com/ProtonMail/go-proton-api.git"
GO_PROTON_API_COMMIT="$(sed -n 's/^ARG GO_PROTON_API_COMMIT=\([0-9a-f]\{40\}\)$/\1/p' "${HARNESS_DIR}/Dockerfile")"

MODE="${PROTON_HARNESS_MODE:-docker}"
STATE_DIR="${PROTON_HARNESS_STATE_DIR:-${REPO_ROOT}/target/proton-harness}"
# Docker bind mounts need an absolute path.
[[ "${STATE_DIR}" == /* ]] || STATE_DIR="${PWD}/${STATE_DIR}"
NATIVE_SRC="${REPO_ROOT}/target/proton-harness-src/${GO_PROTON_API_COMMIT}"
READY_TIMEOUT="${PROTON_HARNESS_READY_TIMEOUT:-120}"

API_PORT=18443
GRPC_PORT=18080
BASE_URL="https://127.0.0.1:${API_PORT}"
CONTAINER="zmail-proton-harness"
# Tagged by the build inputs, so a changed Dockerfile or patch rebuilds.
IMAGE="zmail-proton-harness:$(cat "${HARNESS_DIR}/Dockerfile" "${HARNESS_DIR}/api-port.patch" | sha256sum | cut -c1-16)"
# Proton rejects API calls without an app version; the fake server only
# checks that one is present.
APP_VERSION="zmail@0.0.0-harness"

# The seeded account. The fake server's domain is proton.local, so the
# primary address is <username>@proton.local.
USERNAME="zmail"
PASSWORD="zmail-proton-password"
ALIAS_ADDRESS="zmail.alias@proton.local"
# name:folder|label
SEED_LABELS=("Work:folder" "Receipts:folder" "Follow up:label" "Family:label")

TLS_DIR="${STATE_DIR}/tls"
CA_PEM="${STATE_DIR}/ca.pem"
STATE_JSON="${STATE_DIR}/state.json"
ENV_FILE="${STATE_DIR}/env"
NATIVE_DIR="${STATE_DIR}/native"
NATIVE_PID="${NATIVE_DIR}/server.pid"
NATIVE_LOG="${NATIVE_DIR}/server.log"

log() {
  echo "proton-harness: $*" >&2
}

die() {
  log "error: $*"
  exit 1
}

require() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 || die "'${tool}' is required but not installed"
  done
}

# retry ATTEMPTS DELAY COMMAND... - runs COMMAND until it succeeds, sleeping
# DELAY * attempt seconds between tries.
retry() {
  local attempts="$1" delay="$2" attempt
  shift 2
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    if "$@"; then
      return 0
    fi
    if ((attempt < attempts)); then
      log "attempt ${attempt}/${attempts} of '$*' failed; retrying in $((delay * attempt))s"
      sleep $((delay * attempt))
    fi
  done
  log "'$*' failed after ${attempts} attempts"
  return 1
}

# --- TLS -------------------------------------------------------------------

# A new CA and server certificate for every start, so no key outlives the
# harness run. The leaf is a plain end-entity certificate (rustls refuses a
# CA certificate as the server certificate, which is what the fake server's
# built-in httptest certificate is).
generate_tls() {
  require openssl
  rm -rf "${TLS_DIR}"
  mkdir -p "${TLS_DIR}"
  local extensions="${TLS_DIR}/server.ext"
  cat > "${extensions}" <<'EOF'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = IP:127.0.0.1, DNS:localhost
EOF
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -days 2 -subj "/CN=zmail proton-harness throwaway CA" \
    -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -keyout "${TLS_DIR}/ca-key.pem" -out "${TLS_DIR}/ca.pem" 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -subj "/CN=127.0.0.1" \
    -keyout "${TLS_DIR}/server-key.pem" -out "${TLS_DIR}/server.csr" 2>/dev/null
  openssl x509 -req -in "${TLS_DIR}/server.csr" -days 2 \
    -CA "${TLS_DIR}/ca.pem" -CAkey "${TLS_DIR}/ca-key.pem" -CAcreateserial \
    -extfile "${extensions}" -out "${TLS_DIR}/server.pem" 2>/dev/null
  # The CA key is no longer needed; nothing else may be signed by this CA.
  rm -f "${TLS_DIR}/ca-key.pem" "${TLS_DIR}/server.csr" "${TLS_DIR}/ca.srl" "${extensions}"
  # The container runs as a non-root user that has to read the key.
  chmod 0644 "${TLS_DIR}/server-key.pem"
  cp "${TLS_DIR}/ca.pem" "${CA_PEM}"
}

# --- Docker mode -----------------------------------------------------------

docker_login() {
  if [[ -n "${DOCKERHUB_USERNAME:-}" && -n "${DOCKERHUB_TOKEN:-}" ]]; then
    echo "${DOCKERHUB_TOKEN}" | docker login --username "${DOCKERHUB_USERNAME}" --password-stdin >/dev/null \
      || log "Docker Hub login failed; pulling anonymously"
  fi
}

docker_build() {
  require docker
  docker info >/dev/null 2>&1 \
    || die "the Docker daemon is not reachable; start it, or use PROTON_HARNESS_MODE=native with a Go toolchain"
  if docker image inspect "${IMAGE}" >/dev/null 2>&1; then
    log "using the existing ${IMAGE} image"
    return
  fi
  docker_login
  log "building ${IMAGE} (go-proton-api ${GO_PROTON_API_COMMIT})"
  retry 3 15 docker build --tag "${IMAGE}" "${HARNESS_DIR}" \
    || die "could not build the harness image; see the docker build output above"
}

docker_start() {
  docker rm --force "${CONTAINER}" >/dev/null 2>&1 || true
  docker run --detach --name "${CONTAINER}" \
    --env GO_PROTON_API_SERVER_LOGGER_ENABLED=1 \
    --publish "127.0.0.1:${API_PORT}:8443" \
    --publish "127.0.0.1:${GRPC_PORT}:8080" \
    --volume "${TLS_DIR}:/tls:ro" \
    "${IMAGE}" \
    --port 8080 --api-port 8443 --tls-cert /tls/server.pem --tls-key /tls/server-key.pem \
    >/dev/null || die "could not start the ${CONTAINER} container"
}

docker_running() {
  [[ "$(docker inspect --format '{{.State.Running}}' "${CONTAINER}" 2>/dev/null)" == "true" ]]
}

docker_logs() {
  docker logs --tail "${1:-200}" "${CONTAINER}" 2>&1 || true
}

docker_stop() {
  docker rm --force "${CONTAINER}" >/dev/null 2>&1 || true
}

# --- Native mode -----------------------------------------------------------

native_build() {
  require go git
  NATIVE_BIN="${NATIVE_SRC}/bin"
  local stamp="${NATIVE_BIN}/patch.sha256" want
  want="$(sha256sum "${HARNESS_DIR}/api-port.patch" | cut -d' ' -f1)"
  if [[ -x "${NATIVE_BIN}/proton-server" && -x "${NATIVE_BIN}/proton-client" \
    && "$(cat "${stamp}" 2>/dev/null)" == "${want}" ]]; then
    return
  fi
  rm -rf "${NATIVE_SRC}"
  mkdir -p "${NATIVE_SRC}/repo" "${NATIVE_BIN}"
  log "building go-proton-api ${GO_PROTON_API_COMMIT} with the local Go toolchain"
  local repo="${NATIVE_SRC}/repo"
  if ! {
    git -C "${repo}" init --quiet \
      && retry 3 5 git -C "${repo}" fetch --quiet --depth 1 "${GO_PROTON_API_REPO}" "${GO_PROTON_API_COMMIT}" \
      && git -C "${repo}" checkout --quiet FETCH_HEAD
  }; then
    die "could not fetch go-proton-api ${GO_PROTON_API_COMMIT}"
  fi
  [[ "$(git -C "${repo}" rev-parse HEAD)" == "${GO_PROTON_API_COMMIT}" ]] \
    || die "go-proton-api checkout is not at ${GO_PROTON_API_COMMIT}"
  git -C "${repo}" apply "${HARNESS_DIR}/api-port.patch" || die "api-port.patch does not apply"
  (cd "${repo}" && CGO_ENABLED=0 GOFLAGS=-mod=readonly retry 3 5 go mod download) \
    || die "could not download go-proton-api's modules"
  if ! (
    cd "${repo}" \
      && export CGO_ENABLED=0 GOFLAGS=-mod=readonly \
      && go build -trimpath -o "${NATIVE_BIN}/proton-server" ./server/cmd/server \
      && go build -trimpath -o "${NATIVE_BIN}/proton-client" ./server/cmd/client
  ); then
    die "could not build go-proton-api"
  fi
  echo "${want}" > "${stamp}"
}

native_start() {
  native_stop
  mkdir -p "${NATIVE_DIR}"
  GO_PROTON_API_SERVER_LOGGER_ENABLED=1 nohup "${NATIVE_BIN}/proton-server" \
    --port "${GRPC_PORT}" --api-port "${API_PORT}" \
    --tls-cert "${TLS_DIR}/server.pem" --tls-key "${TLS_DIR}/server-key.pem" \
    >"${NATIVE_LOG}" 2>&1 &
  echo $! > "${NATIVE_PID}"
}

native_running() {
  [[ -f "${NATIVE_PID}" ]] && kill -0 "$(cat "${NATIVE_PID}")" 2>/dev/null
}

native_logs() {
  tail -n "${1:-200}" "${NATIVE_LOG}" 2>/dev/null || true
}

native_stop() {
  if native_running; then
    kill "$(cat "${NATIVE_PID}")" 2>/dev/null || true
  fi
  rm -f "${NATIVE_PID}"
}

# --- Mode dispatch -----------------------------------------------------------

case "${MODE}" in
  docker | native) ;;
  *) die "PROTON_HARNESS_MODE must be docker or native, not '${MODE}'" ;;
esac

server_build() { "${MODE}_build"; }
server_start() { "${MODE}_start"; }
server_running() { "${MODE}_running"; }
server_logs() { "${MODE}_logs" "$@"; }
server_stop() { "${MODE}_stop"; }

# control ARGS... - runs go-proton-api's gRPC control client. The client
# sets no deadline of its own, so each call gets one here (where coreutils'
# timeout exists) and a wedged server fails the run instead of hanging it.
control() {
  local limit=()
  command -v timeout >/dev/null 2>&1 && limit=(timeout 30)
  if [[ "${MODE}" == docker ]]; then
    ${limit[@]+"${limit[@]}"} docker exec "${CONTAINER}" /usr/local/bin/proton-client --host 127.0.0.1 --port 8080 "$@"
  else
    native_build
    ${limit[@]+"${limit[@]}"} "${NATIVE_BIN}/proton-client" --host 127.0.0.1 --port "${GRPC_PORT}" "$@"
  fi
}

# --- Readiness and seeding ---------------------------------------------------

api_ping() {
  curl --silent --show-error --fail --max-time 5 --noproxy '*' \
    --cacert "${CA_PEM}" --header "x-pm-appversion: ${APP_VERSION}" \
    "${BASE_URL}/tests/ping" >/dev/null 2>&1
}

wait_until_ready() {
  local deadline=$((SECONDS + READY_TIMEOUT)) last="nothing checked yet"
  while ((SECONDS < deadline)); do
    if ! server_running; then
      log "the server exited before it became ready; its log:"
      server_logs >&2
      die "harness server exited during startup"
    fi
    if ! control info >/dev/null 2>&1; then
      last="gRPC control service on 127.0.0.1:${GRPC_PORT} not answering"
    elif ! api_ping; then
      last="API ${BASE_URL}/tests/ping not answering over TLS"
    else
      log "ready: API ${BASE_URL}, gRPC 127.0.0.1:${GRPC_PORT}"
      return 0
    fi
    sleep 1
  done
  log "not ready after ${READY_TIMEOUT}s (last check: ${last}); server log:"
  server_logs >&2
  # One verbose probe, so the TLS or connection error is in the CI log.
  curl --verbose --max-time 5 --noproxy '*' --cacert "${CA_PEM}" \
    --header "x-pm-appversion: ${APP_VERSION}" "${BASE_URL}/tests/ping" >&2 || true
  die "harness did not become ready within ${READY_TIMEOUT}s"
}

# json_field JSON FIELD - prints one string field of the control client's
# JSON output, failing loudly when it is missing.
json_field() {
  local value
  value="$(jq -er --arg field "$2" '.[$field] // empty' <<<"$1")" \
    || die "control client returned no $2: $1"
  printf '%s' "${value}"
}

seed() {
  require jq
  server_running || die "the harness is not running; start it with 'scripts/proton-harness.sh up'"
  [[ -f "${CA_PEM}" ]] || die "${CA_PEM} is missing; restart the harness with 'scripts/proton-harness.sh reset'"
  if [[ -f "${STATE_JSON}" ]]; then
    die "the harness is already seeded (${STATE_JSON}); use 'reset' for a fresh account"
  fi

  # Each call runs once: retrying a call that half-succeeded would only meet
  # "already exists" (see #201). A failed seed fails `up`, and
  # ProtonHarness::start retries with a whole fresh harness (`reset`).
  local output user_id primary_id alias_id labels="[]" entry name kind flag label_id
  output="$(control user create --username "${USERNAME}" --password "${PASSWORD}")" \
    || die "could not create the ${USERNAME} user: ${output}"
  user_id="$(json_field "${output}" userID)"
  primary_id="$(json_field "${output}" addrID)"

  output="$(control address create --userID "${user_id}" --email "${ALIAS_ADDRESS}" --password "${PASSWORD}")" \
    || die "could not create ${ALIAS_ADDRESS}: ${output}"
  alias_id="$(json_field "${output}" addrID)"

  for entry in "${SEED_LABELS[@]}"; do
    name="${entry%:*}"
    kind="${entry##*:}"
    flag=()
    [[ "${kind}" == folder ]] && flag=(--exclusive)
    output="$(control label create --userID "${user_id}" --name "${name}" ${flag[@]+"${flag[@]}"})" \
      || die "could not create the ${name} ${kind}: ${output}"
    label_id="$(json_field "${output}" labelID)"
    labels="$(jq -c --arg name "${name}" --arg kind "${kind}" --arg id "${label_id}" \
      '. + [{name: $name, kind: $kind, id: $id}]' <<<"${labels}")"
  done

  jq -n \
    --arg base_url "${BASE_URL}" \
    --arg ca_pem "${CA_PEM}" \
    --arg app_version "${APP_VERSION}" \
    --arg username "${USERNAME}" \
    --arg password "${PASSWORD}" \
    --arg user_id "${user_id}" \
    --arg primary "${USERNAME}@proton.local" \
    --arg primary_id "${primary_id}" \
    --arg alias "${ALIAS_ADDRESS}" \
    --arg alias_id "${alias_id}" \
    --argjson labels "${labels}" \
    '{
      base_url: $base_url,
      ca_pem: $ca_pem,
      app_version: $app_version,
      username: $username,
      password: $password,
      user_id: $user_id,
      addresses: [
        {email: $primary, id: $primary_id},
        {email: $alias, id: $alias_id}
      ],
      labels: $labels
    }' > "${STATE_JSON}.tmp"
  mv "${STATE_JSON}.tmp" "${STATE_JSON}"

  cat > "${ENV_FILE}" <<EOF
export PROTON_HARNESS_URL='${BASE_URL}'
export PROTON_HARNESS_CA_PEM='${CA_PEM}'
export PROTON_HARNESS_USERNAME='${USERNAME}'
export PROTON_HARNESS_PASSWORD='${PASSWORD}'
export PROTON_HARNESS_STATE='${STATE_JSON}'
EOF
  log "seeded ${USERNAME} (${USERNAME}@proton.local, ${ALIAS_ADDRESS}) with ${#SEED_LABELS[@]} labels; state in ${STATE_JSON}"
}

up() {
  [[ -n "${GO_PROTON_API_COMMIT}" ]] || die "could not read GO_PROTON_API_COMMIT from ${HARNESS_DIR}/Dockerfile"
  require curl jq
  server_build
  down
  mkdir -p "${STATE_DIR}"
  generate_tls
  server_start
  wait_until_ready
  seed
}

down() {
  server_stop
  rm -rf "${TLS_DIR}" "${CA_PEM}" "${STATE_JSON}" "${ENV_FILE}"
}

case "${1:-up}" in
  up) up ;;
  seed) seed ;;
  down) down ;;
  reset) down && up ;;
  build)
    [[ -n "${GO_PROTON_API_COMMIT}" ]] || die "could not read GO_PROTON_API_COMMIT from ${HARNESS_DIR}/Dockerfile"
    server_build
    ;;
  env)
    [[ -f "${ENV_FILE}" ]] || die "the harness is not up; run 'scripts/proton-harness.sh up'"
    cat "${ENV_FILE}"
    ;;
  logs) server_logs 100000 ;;
  ps)
    if server_running; then
      echo "running (${MODE}): API ${BASE_URL}, gRPC 127.0.0.1:${GRPC_PORT}"
    else
      echo "not running (${MODE})"
      exit 1
    fi
    ;;
  *)
    echo "usage: scripts/proton-harness.sh [up|seed|down|reset|build|env|logs|ps]" >&2
    exit 1
    ;;
esac
