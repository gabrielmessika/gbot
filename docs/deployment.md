# gbot Deployment Guide

## Prerequisites

- Docker installed
- Hyperliquid API wallet (agent wallet) configured

## Environment Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `GBOT__EXCHANGE__WALLET_ADDRESS` | Hyperliquid wallet address | Yes (live) |
| `GBOT__EXCHANGE__AGENT_PRIVATE_KEY` | Agent wallet private key (hex) | Yes (live) |
| `GBOT__GENERAL__MODE` | `observation`, `dry-run`, or `live` | No (default: observation) |
| `RUST_LOG` | Log level (`info`, `debug`, `warn`) | No (default: info) |

## Running locally (observation mode)

```bash
cargo run
```

## Running with Docker

```bash
docker build -t gbot .
docker run -d \
  --name gbot \
  -p 3000:3000 \
  -v $(pwd)/data:/app/data \
  -e GBOT__GENERAL__MODE=observation \
  gbot
```

## Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check |
| `GET /metrics` | Prometheus metrics |
| `GET /api/status` | Bot status JSON |

## Data Directory Structure

```
data/
├── l2/{coin}/{date}.jsonl       — Book snapshots (JSONL, input backtest)
├── trades/{coin}/{date}.jsonl   — Trade tape (JSONL, input backtest)
├── features/{coin}/{date}.jsonl — Computed features
└── journal/journal_{ts}.jsonl   — Order journal
```

## Backtest

```bash
# Run backtest on recorded data for a specific date
cargo run --release -- --backtest --date 2024-11-15 --coins BTC,ETH

# Convert JSONL to Parquet (offline analysis)
cargo run --release -- --convert-parquet --coin BTC --date 2024-11-15
```

Backtest input: `data/l2/{coin}/{date}.jsonl` + `data/trades/{coin}/{date}.jsonl`
Output: `BacktestSummary` printed to stdout (win rate, P&L, fill rate, drawdown).

## Monitoring

- Prometheus metrics available at `:3000/metrics`
- Kill-switch triggers alerts (when webhook is configured)
- Check logs with `docker logs gbot`
