#
# ouyi-api production image
# ------------------------
# Stages:
#   1. web-builder   — build the React/Vite admin SPA (web/dist)
#   2. chef          — serialize Cargo dependency graph via cargo-chef (cacheable)
#   3. builder       — cook the recipe + build release binary with sccache/cache mounts
#   4. runtime       — debian-slim + tini + non-root user; COPY binary + web/dist in
#
# Build requirements: BuildKit enabled (Docker 20.10+, or `BUILDKIT=1`).
# Build (example):
#   DOCKER_BUILDKIT=1 docker build -t ouyi-api:dev -f Dockerfile .
#
# Runtime expectations:
#   * config.toml mounted at /app/config/config.toml (or OUYI_CONFIG env override)
#   * SERVER_PORT env var chooses the listen port (default 3000)
#

############################
# 1. Web builder
############################
FROM node:22-bookworm-slim AS web-builder

WORKDIR /web

# Install deps in a separate layer so package-lock changes do not invalidate
# the production node_modules cache.
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci --no-audit --no-fund

# Build the SPA. tsc -b && vite build is the project-defined production build
# (see web/package.json scripts.build).
COPY web/ ./
RUN npm run build

############################
# 2. cargo-chef cooker
############################
FROM rust:1-slim-bookworm AS chef

RUN cargo install --locked cargo-chef --root /usr/local

WORKDIR /app

############################
# 3. Dependency cooking + release build
############################
FROM chef AS builder

# Native deps reqwest + openssl-sys + chrono pull in during link.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Re-emit the recipe from the now-stable source tree.
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Prepare dependency recipe.
RUN cargo chef prepare --recipe-path recipe.json

# Cook deps in their own layer; dependencies rarely change so this is the
# long-lived cache hit.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry \
    --mount=type=cache,target=/root/.cargo/git,id=cargo-git \
    --mount=type=cache,target=/app/target,id=cargo-target \
    cargo chef cook --release --recipe-path recipe.json

# Build the actual binary. Same cache mounts so rebuilt-without-dependency-
# change layers stay quick.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry \
    --mount=type=cache,target=/root/.cargo/git,id=cargo-git \
    --mount=type=cache,target=/app/target,id=cargo-target \
    CARGO_INCREMENTAL=0 \
    cargo build --release --locked \
 && strip target/release/ouyi-api \
 && ls -l target/release/ouyi-api \
 && cp /app/target/release/ouyi-api /app/ouyi-api-binary

# Cache mounts are not visible to COPY --from=, so the binary was copied
# into the writable layer in the same RUN step as cargo build above.

############################
# 4. Runtime
############################
FROM debian:bookworm-slim AS runtime

# Runtime libs the link line above depended on, plus tini for signal handling
# (main.rs already handles SIGTERM; tini ensures PID 1 forwards signals).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        libssl3 ca-certificates tini wget \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 1000 app \
 && useradd  --system --uid 1000 --gid app --no-create-home --shell /usr/sbin/nologin app

ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    SERVER_HOST=0.0.0.0 \
    SERVER_PORT=3000 \
    OUYI_CONFIG=/app/config/config.toml \
    RUST_LOG=info

WORKDIR /app

# Layer ordering: large blobs last; web/dist may be empty if the front-end
# build is skipped, but the static_file_router falls back to rust-embed
# (see src/admin/mod.rs serve_embedded_fallback) so the admin SPA still loads.
COPY --from=web-builder /web/dist ./web/dist
COPY --from=builder   /app/ouyi-api-binary ./ouyi-api

# config.toml bind-mount target; mkdir so the volume mount is clean.
RUN mkdir -p /app/config && chown -R app:app /app
VOLUME ["/app/config"]
EXPOSE 3000

USER app

# /v1/models is a cheap, auth-free GET endpoint registered in src/router.rs.
# curl-style probe with wget avoids pulling in curl into the slim image.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -qO- "http://127.0.0.1:${SERVER_PORT}/v1/models" >/dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["./ouyi-api"]
