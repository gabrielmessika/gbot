# Changelog

## 2026-04-01 — Dashboard V2 + Logging persistant

- **Dashboard V2** : 4 onglets (Status, Positions, Books, Events). Onglet Status = vue par défaut avec santé du bot, performance session, tableau par période (1h/24h/7j), historique des trades fermés (P&L, raison, break-even)
- **Backend** : `ClosedTradeView`, `BotStatusView` ajoutés au `DashboardSnapshot`. Tracking des trades fermés, erreurs/warnings, stats par période calculées à chaque tick
- **Logging persistant** : `tracing-appender` avec rotation quotidienne dans `data/logs/`. Dual output (stdout JSON + fichiers). Docker log rotation (50Mo × 10 fichiers)
- **deploy.sh** : volume `logs/` monté, `--log-driver json-file --log-opt max-size=50m --log-opt max-file=10`
- **fetch-data.sh** : récupère aussi les logs applicatifs persistés (`data/logs/`)
- **Docs** : guide d'interprétation de l'UI dans `deployment.md` section 3

## 2026-04-01 — fetch-data.sh

- **fetch-data.sh**: Script de récupération des données du serveur (rsync). Filtrage par date/jours, mode --logs-only, --dry-run. Récupère aussi un snapshot de l'API et les logs Docker.

## 2026-04-01 — Dashboard UI + Deployment Scripts

- **Dashboard UI** (`static/`): Single page HTML/JS/CSS (dark theme), SSE temps réel (500ms), 5 zones (header, books, positions, métriques, event feed)
- **Backend SSE** (`dashboard.rs`): `DashboardSnapshot` via `Arc<RwLock<>>`, `EventFeed` (rolling 30 events), routes `/api/state` + `/api/stream`, `ServeDir` pour fichiers statiques
- **Main loop**: dashboard tick toutes les 500ms dans le `select!`, tracking des régimes par coin, event push (fills, régime changes, reconnects, risk rejections)
- **deploy.sh**: rsync + docker build + auto (re)start avec `--start`, health check
- **prepareServer.sh**: Docker, fail2ban, ufw, utilisateur gbot-deploy, limites nofile, sécurisation SSH
- **Cargo.toml**: ajout `tower-http` feature `fs`, `tokio-stream`
- **Dockerfile**: copie `static/` dans l'image

## 2026-04-01 — Initial Implementation

- Full project scaffold based on plan3.md (MFDP V1)
- **Config**: TOML-based settings with env var overrides (`GBOT__` prefix)
- **Exchange**: WebSocket client (reconnect, heartbeat, backoff), REST client (rate-limited), EIP-712 signer, token-bucket rate limiter with real Hyperliquid weights
- **Market Data**: Local order book (BTreeMap), book manager (snapshots + deltas), trade tape (ring buffer), JSONL recorder
- **Features**: Book features (spread, imbalance, micro-price, VAMP, depth), flow features (OFI multi-window, volatility, trade intensity, toxicity proxy)
- **Regime Engine**: 8 market regimes (QuietTight, QuietThin, ActiveHealthy, ActiveToxic, WideSpread, NewslikeChaos, LowSignal, DoNotTrade)
- **Strategy**: MFDP V1 — directional score from microstructure features, queue desirability scoring, ALO entry
- **Execution**: Order manager (state machine per coin, client OID convention), position manager (lifecycle, break-even, trailing, recovery, orphan cleanup)
- **Risk Manager**: Absolute veto, drawdown circuit breaker, throttle tiers, equity spike guard, daily reset, kill-switch
- **Portfolio**: Internal state tracking (realized PnL, fees, funding)
- **Persistence**: JSONL journal, Parquet writer placeholder
- **Observability**: Prometheus metrics, Axum dashboard (health, metrics, status)
- **Backtest**: Replay engine, sim book, sim execution (probabilistic fill model, fee model)
- All 15 t-bot/tbot-scalp bugs incorporated as design protections
