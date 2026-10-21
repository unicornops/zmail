# Proton API test double: go-proton-api's fake server (server/cmd/server)
# and its gRPC control client (server/cmd/client), built at the pinned
# commit with api-port.patch and srp-salt.patch applied. Test-only; nothing
# from this image ships in zmail. See README.md and scripts/proton-harness.sh.

# golang:1.26.7-bookworm (multi-arch index digest). go-proton-api's go.mod
# asks for toolchain go1.26.7; GOTOOLCHAIN=local keeps the build on this
# image's toolchain instead of downloading another one.
FROM golang:1.26.7-bookworm@sha256:e8c859f5632dcfde7b32d2012b4351728f6437930887c2f6a91ea242459e5514 AS build

ARG GO_PROTON_API_COMMIT=8a62023d3e38cb29749bcfc01bf44eafda6cec82

ENV CGO_ENABLED=0 \
    GOFLAGS=-mod=readonly \
    GOTOOLCHAIN=local

WORKDIR /src/go-proton-api

RUN git init --quiet . \
    && git fetch --quiet --depth 1 https://github.com/ProtonMail/go-proton-api.git "${GO_PROTON_API_COMMIT}" \
    && git checkout --quiet FETCH_HEAD \
    && test "$(git rev-parse HEAD)" = "${GO_PROTON_API_COMMIT}"

# Module downloads are checked against upstream's go.sum.
RUN go mod download

COPY api-port.patch srp-salt.patch /src/
RUN git apply /src/api-port.patch \
    && git apply /src/srp-salt.patch

RUN go build -trimpath -o /out/proton-server ./server/cmd/server \
    && go build -trimpath -o /out/proton-client ./server/cmd/client

# gcr.io/distroless/static-debian12:nonroot (multi-arch index digest).
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab

COPY --from=build /out/proton-server /out/proton-client /usr/local/bin/

# 8080: gRPC control service (create users, addresses, labels).
# 8443: the Proton API, over TLS when started with --tls-cert/--tls-key.
EXPOSE 8080 8443

ENTRYPOINT ["/usr/local/bin/proton-server"]
CMD ["--port", "8080", "--api-port", "8443"]
