# syntax=docker/dockerfile:1.7
# ──────────────────────────────────────────────────────────────────────────────
# Cross-compilation helper
# tonistiigi/xx provides xx-* wrappers that set the right Rust target triple,
# linker, and sysroot for any TARGETPLATFORM without QEMU emulation.
# ──────────────────────────────────────────────────────────────────────────────
FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.6.1 AS xx

# ──────────────────────────────────────────────────────────────────────────────
# Build stage — always runs on the BUILD machine's native arch (no emulation).
# TARGETPLATFORM is injected by `docker buildx`; for plain `docker build` it
# defaults to the host arch so local builds work without any extra flags.
# ──────────────────────────────────────────────────────────────────────────────
FROM --platform=$BUILDPLATFORM rust:1.86-alpine AS builder

# Copy xx cross-compilation helpers into the builder image.
COPY --from=xx / /

# Build arguments injected by buildx.
ARG TARGETPLATFORM
ARG TARGETARCH

# musl-dev  – C stdlib headers for static linking
# pkgconfig – used by some crate build scripts
# xx-apk    – installs the target-arch sysroot when cross-compiling
RUN apk add --no-cache musl-dev pkgconfig \
    && xx-apk add --no-cache musl-dev

# Register the Rust target triple for the requested platform.
# xx-info --rust-target-triple outputs e.g. aarch64-unknown-linux-musl.
RUN rustup target add "$(xx-info --rust-target-triple)"

# ── Dependency cache layer ────────────────────────────────────────────────────
# Copy manifests first so the expensive `cargo fetch` step is cached unless
# Cargo.toml or Cargo.lock changes.  Plain `cargo fetch` is used here — no
# cross-compilation needed just to download crate sources.
WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
    && cargo fetch \
    && rm -rf src

# ── Full build ────────────────────────────────────────────────────────────────
COPY . .

# xx-cargo cross-compiles for TARGETPLATFORM without QEMU.
# reqwest uses rustls-tls so there is no OpenSSL runtime dependency.
RUN xx-cargo build --release \
    && cp "target/$(xx-info --rust-target-triple)/release/panw-api-ollama" /panw-api-ollama \
    && xx-verify /panw-api-ollama

# ──────────────────────────────────────────────────────────────────────────────
# Runtime stage — minimal Alpine (~7 MB), runs on any Linux distro / any arch.
# ──────────────────────────────────────────────────────────────────────────────
FROM alpine:3.21

# ca-certificates – TLS trust store for outbound PANW API calls
# tzdata         – lets operators set TZ env var for localised log timestamps
RUN apk add --no-cache ca-certificates tzdata \
    && update-ca-certificates

# Non-root user for least-privilege execution.
RUN addgroup -S appgroup && adduser -S appuser -G appgroup

WORKDIR /app
COPY --from=builder /panw-api-ollama .
RUN chown appuser:appgroup /app/panw-api-ollama

# All values are overridable at runtime via environment variables or .env file.
ENV SERVER_HOST="0.0.0.0" \
    SERVER_PORT=11435 \
    SERVER_DEBUG_LEVEL="INFO" \
    OLLAMA_BASE_URL="http://ollama:11434" \
    SECURITY_BASE_URL="https://service.api.aisecurity.paloaltonetworks.com" \
    SECURITY_API_KEY="" \
    SECURITY_PROFILE_NAME="" \
    SECURITY_APP_NAME="panw-api-ollama" \
    SECURITY_APP_USER="docker"

# SECURITY_API_KEY and SECURITY_PROFILE_NAME must be injected at runtime via
# docker-compose env_file or --env-file; never bake secrets into the image.

USER appuser
EXPOSE 11435

# Lightweight liveness probe — does NOT call Ollama.
# wget (BusyBox) is always available in Alpine.
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -qO- http://127.0.0.1:11435/healthz >/dev/null 2>&1 || exit 1

CMD ["./panw-api-ollama"]
