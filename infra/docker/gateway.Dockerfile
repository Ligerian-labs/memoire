# memoire gateway for container platforms such as Dokploy.
# Build context: the repository root (Cargo workspace, crates/, guests/).
# The image carries the gateway, the catalog guest, the runtime manifest, the
# application document and the production configuration
# (infra/prod/memoire.toml). Every secret comes from the environment
# (docs/deploy/dokploy.md).
FROM rust:1.98.0-bookworm AS build
RUN apt-get update \
  && apt-get install -y --no-install-recommends cmake clang \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
# rust-toolchain.toml adds the wasm32-unknown-unknown target the guests need.
RUN rustup show active-toolchain
COPY crates ./crates
COPY guests ./guests
RUN cargo build --release --locked -p memoire-gateway
# The catalog guest, built as scripts/build-guests.sh does.
RUN target/release/memoire-gateway build \
    --manifest guests/runtime.toml --workspace guests/Cargo.toml \
  && test -s guests/components/memoire-catalog.wasm

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --uid 10001 --home-dir /var/lib/memoire memoire \
  && mkdir -p /etc/memoire/guests/components /var/lib/memoire \
  && chown -R memoire:memoire /var/lib/memoire
COPY --from=build /src/target/release/memoire-gateway /usr/local/bin/memoire-gateway
COPY --from=build /src/guests/runtime.toml /etc/memoire/guests/runtime.toml
COPY --from=build /src/guests/components/memoire-catalog.wasm /etc/memoire/guests/components/memoire-catalog.wasm
# The catalog application is the same document in development and production.
COPY config/dev/application.toml /etc/memoire/application.toml
COPY infra/prod/memoire.toml /etc/memoire/memoire.toml
USER memoire
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/health/ready || exit 1
ENTRYPOINT ["memoire-gateway"]
CMD ["run", "--config", "/etc/memoire/memoire.toml"]
