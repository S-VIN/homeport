# syntax=docker/dockerfile:1
# Build from the repository root: docker build -f docker/Dockerfile .
# rust-toolchain.toml is the source of truth for Rust: rustup installs that
# version automatically if this image tag ever falls behind it.
FROM rust:1.99.1-bookworm AS build
WORKDIR /src
COPY . .
ARG TARGETARCH
# Cache mounts keep the registry, target/ and the wasm-bindgen CLI between local builds.
# The server binary drops its symbol table too (about 10 MB); panics still report file:line.
RUN --mount=type=cache,id=rproxy-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=rproxy-target-${TARGETARCH},target=/src/target,sharing=locked \
    --mount=type=cache,id=rproxy-tools-${TARGETARCH},target=/src/.cache,sharing=locked \
    python3 scripts/build.py --release --no-default-features --config=profile.release.strip=true \
    && cp target/release/rproxy /rproxy \
    && python3 scripts/mihomo.py --os linux --arch "$TARGETARCH" --output /core

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir /data
WORKDIR /app
COPY --from=build /rproxy ./rproxy
COPY --from=build /core/ ./assets/mihomo/
ENV HOME=/data
ENTRYPOINT ["/app/rproxy"]
CMD ["web", "--listen", "127.0.0.1:20800", "--data-dir", "/data"]
