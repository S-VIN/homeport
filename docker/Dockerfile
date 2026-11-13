# syntax=docker/dockerfile:1
FROM --platform=$BUILDPLATFORM rust:1.99.1-bookworm AS assets
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN python3 scripts/build.py --web-only

# The web painter and core selection run natively even for an ARM image.
ARG TARGETARCH
RUN python3 scripts/mihomo.py --os linux --arch "$TARGETARCH" --output /core

FROM rust:1.99.1-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=assets /src/web/pkg/ web/pkg/
RUN cargo build --release --locked --no-default-features --bin rproxy

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir /data
WORKDIR /app
COPY --from=build /src/target/release/rproxy ./rproxy
COPY --from=assets /core/ ./assets/mihomo/
ENV HOME=/data
ENTRYPOINT ["/app/rproxy"]
CMD ["web", "--listen", "127.0.0.1:20800", "--data-dir", "/data"]
