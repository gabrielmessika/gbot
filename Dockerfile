FROM rust:1.77-slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ ./src/

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/gbot .
COPY config/ ./config/
COPY static/ ./static/

RUN mkdir -p data/l2 data/trades data/features data/signals data/orders data/fills data/pnl data/journal data/logs

EXPOSE 3000

ENV RUST_LOG=info
ENV GBOT__GENERAL__MODE=dry-run

CMD ["./gbot"]
