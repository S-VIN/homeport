# syntax=docker/dockerfile:1
# Build from the repository root: docker build -f docker/Dockerfile .
# CI builds the binaries on the runner and replaces the `build` stage with
# `--build-context build=<dir>` holding homeport, core/mihomo, core/LICENSE and
# presets/server.sql.
# rust-toolchain.toml is the source of truth for Rust: rustup installs that
# version automatically if this image tag ever falls behind it.
FROM rust:1.100.0-trixie AS build
WORKDIR /src
COPY . .
ARG TARGETARCH
# Cache mounts keep the registry, target/ and the wasm-bindgen CLI between local builds.
# The server binary drops its symbol table too (about 10 MB); panics still report file:line.
RUN --mount=type=cache,id=homeport-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=homeport-target-${TARGETARCH},target=/src/target,sharing=locked \
    --mount=type=cache,id=homeport-tools-${TARGETARCH},target=/src/.cache,sharing=locked \
    python3 scripts/build.py --release --no-default-features --config=profile.release.strip=true \
    && cp target/release/homeport /homeport \
    && python3 scripts/mihomo.py --os linux --arch "$TARGETARCH" --output /core \
    && mkdir /presets && cp presets/server.sql /presets/

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir /data
WORKDIR /app
COPY --from=build /homeport ./homeport
COPY --from=build /core/mihomo ./mihomo
COPY --from=build /core/LICENSE ./licenses/mihomo/LICENSE
# A new installation starts as a VLESS server (presets/server.sql).
COPY --from=build /presets/server.sql ./preset.sql
ENV HOME=/data
# Plugins see Platform::Docker: no system proxy, TUN or login items here.
ENV HOMEPORT_DOCKER=1 HOMEPORT_DATA_DIR=/data HOMEPORT_LISTEN=127.0.0.1:20800
ENTRYPOINT ["/app/homeport"]
CMD ["web"]
