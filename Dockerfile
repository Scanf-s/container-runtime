FROM rust:1-slim-bookworm

RUN apt-get update && \
    apt-get install -y --no-install-recommends curl ca-certificates iproute2 procps util-linux && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
