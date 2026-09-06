# syntax=docker/dockerfile:1.7
#
# Uptime Medics: multi-stage build producing a small, non-root Debian image with a single
# statically-linked-enough Rust binary (SQLite is bundled, TLS via rustls; no OpenSSL needed).

# ---------- build stage ----------
FROM rust:1-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY static ./static
COPY src ./src

# Cache the cargo registry and the target directory between builds (BuildKit cache mounts).
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked \
 && cp target/release/uptime-medics /usr/local/bin/uptime-medics

# ---------- runtime stage ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 medic \
 && useradd --system --uid 10001 --gid medic --home-dir /data --shell /usr/sbin/nologin medic \
 && mkdir -p /data \
 && chown medic:medic /data

COPY --from=builder /usr/local/bin/uptime-medics /usr/local/bin/uptime-medics

ENV HOST=0.0.0.0 \
    PORT=8080 \
    DATABASE_URL=sqlite:///data/uptime_medics.db \
    RUST_LOG=info

USER medic
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["uptime-medics", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/uptime-medics"]
