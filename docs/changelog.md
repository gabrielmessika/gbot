# Changelog

## 2026-04-01 — Phases 7.4 / 7.5 / 7.6 implementation

### Phase 7.4 — Analyse offline Python (`scripts/analyze_dry_run.py`)
- Script autonome (pas de dépendances externes) qui charge les fichiers JSONL enregistrés
- 7 sections : distribution features, corrélation Spearman feature×mid_move, adverse selection,
  sensibilité SL/TP paramétrique, performance par coin, performance par heure UTC, recommandations
- Usage : `python scripts/analyze_dry_run.py --data-dir ./data [--date 2026-04-01] [--output report.txt]`

### Phase 7.5 — Entry timing pullback (`src/strategy/pullback.rs`)
- State machine par coin : `WaitingMove` → `WaitingPullback` → `Ready | Abandoned`
- Séquence : direction_score confirmé → micro-move ≥ min_move_bps → retrace ≥ retrace_pct → OFI confirme
- Abandon : timeout (max_wait_pullback_s), renversement (retrace > 100%), signal opposé
- Config : `pullback_min_move_bps=3.0`, `pullback_retrace_pct=0.35`, `pullback_ofi_confirm=0.0`
- Entrée recalculée au mid du pullback (SL/TP en % hérités du signal initial)
- Signal cooldown posé au moment de l'entrée effective (pas au moment du signal directionnel)

### Phase 7.6 — Backtest amélioré (`src/backtest/runner.rs`, `replay_engine.rs`)
- MAE/MFE tracker tick-by-tick : `mae_bps`, `mfe_bps` par trade (pire adversité / meilleur gain)
- Adverse selection : `adverse_5s` — mid a-t-il bougé contre la direction dans les 5s post-entrée ?
- Summary : `avg_mae_bps`, `avg_mfe_bps`, `mae_to_sl_ratio` (> 1.0 = SL trop serré vs bruit)
- Mode comparaison : `run_comparison(fixed_sl_bps)` → `ComparisonResult { dynamic_sl, fixed_sl, deltas }`
- Mode SL fixe : `SlMode::Fixed(bps)` override SL/TP pour rejouer avec SL constant
- `initial_equity()` getter sur `ReplayEngine` pour les runs multiples

## 2026-04-01 — Post dry-run v1: 6 critical fixes

Analysis of the first dry-run session (53 signals, 12 trades, WR=25%, P&L=-$98) revealed 6 systematic issues. All fixed in this release.

### 1. Book inversion fix (spread_bps negative — 96% of signals)
- **Root cause**: L2 delta application could accumulate crossed levels (bid ≥ ask) over time. The book progressively corrupted: spread went from -1.4 bps at startup to -120 bps after 30min.
- **Fix**: `OrderBook::apply_delta()` now calls `sanitize_crossed()` after every delta application. This removes bid levels ≥ best ask and ask levels ≤ best bid. `spread_bps()` returns `None` if ask ≤ bid. `BookManager` marks the book as stale if still crossed after sanitization.
- **Guard**: `compute_book_features()` returns defaults for crossed books. Strategy skips coins with `spread_bps ≤ 0`.
- **Files**: `book.rs`, `book_manager.rs`, `book_features.rs`, `mfdp.rs`

### 2. OFI saturation fix (55% of values at ±1.0)
- **Root cause**: `(buy_vol - sell_vol) / total` saturates to ±1 easily when a 10s window contains only 2-3 trades in the same direction (common in crypto).
- **Fix**: Added confidence scaling — OFI magnitude is reduced when fewer than `MIN_OFI_TRADES` (5) trades exist in the window: `raw_ofi × min(trade_count/5, 1.0)`. This prevents a single trade from generating OFI=±1.0.
- **File**: `flow_features.rs`

### 3. Aggression persistence fix (always ≥ 0.5, never negative)
- **Root cause**: Formula was `max(buys, sells) / total` — by definition, `max(a, b) ≥ ceil(total/2)`, so the minimum was always 0.5. The feature measured concentration, not directional persistence.
- **Fix**: Changed to signed ratio: `(2 × buy_count - total) / total`. Range is now [-1, +1]: +1 = all buys, -1 = all sells, 0 = balanced. The direction_score formula in `mfdp.rs` no longer needs to infer sign from OFI.
- **Files**: `flow_features.rs`, `mfdp.rs`

### 4. Feature maturity guard (vol_ratio=0, trades=0)
- **Root cause**: 42% of signals had `vol_ratio=0.0` because `realized_vol_30s` had no data yet. The bot traded on immature features.
- **Fix**: Added `FlowFeatures::is_mature()` method requiring `trade_count_10s ≥ 5` AND `realized_vol_30s > 0`. Strategy and main loop skip evaluation when features are not mature.
- **New field**: `trade_count_10s: usize` in `FlowFeatures`.
- **New config**: `min_trades_for_signal = 5` (configurable).
- **Files**: `flow_features.rs`, `mfdp.rs`, `main.rs`, `settings.rs`, `default.toml`

### 5. Dynamic SL/TP based on realized volatility
- **Root cause**: SL was fixed at 0.30% (`pullback_retrace_pct`) for ALL coins regardless of volatility. 0.30% = 30 bps ≈ normal noise for most coins. Result: 11/12 trades hit SL within seconds of entry.
- **Fix**: SL distance is now `max(sl_min_bps, sl_vol_multiplier × realized_vol_30s)`, capped at `sl_max_bps`. TP = SL × `target_rr`. `pullback_retrace_pct` is kept for entry retrace detection only, no longer used for SL.
- **Defaults**: `sl_vol_multiplier=2.5`, `sl_min_bps=15`, `sl_max_bps=80`, `target_rr=2.0`
- **Files**: `mfdp.rs`, `settings.rs`, `default.toml`

### 6. Direction score confirmation (avg |dir| was 0.549, threshold 0.50)
- **Root cause**: Signals were barely above the 0.50 threshold (average 0.549). A single noisy tick crossing the threshold would trigger a trade.
- **Fix**: Added confirmation counter per coin. The direction score must remain above threshold for `min_direction_confirmations` consecutive evaluations (default: 3) before a signal is emitted. Counter resets when direction changes.
- **New config**: `min_direction_confirmations = 3`
- **Files**: `main.rs`, `settings.rs`, `default.toml`

## 2026-04-01 — Bugfixes prix d'ordre & doctest

### Prix d'ordre incorrects pour DOGE, XRP, OP (bloquant)
- **Cause** : `from_exchange_meta` utilisait `szDecimals` (précision des *tailles*) comme `tick_size` pour arrondir les *prix*. Sur Hyperliquid, `szDecimals=0` pour DOGE et XRP signifie "quantités en unités entières" — rien à voir avec le prix. `round_price_to_tick(0.0923, tick=1.0)` donnait `0` ; `round_price_to_tick(1.35, tick=1.0)` donnait `1`.
- **Fix** : `round_price_to_tick` utilise désormais un arrondi à **5 chiffres significatifs** (convention Hyperliquid pour les prix), sans dépendre du paramètre `tick`. Exemples après fix : DOGE `0.092328` → `0.092328`, XRP `1.3504` → `1.3504`, BTC `68594` → `68594`.
- **Fichier** : `src/config/coins.rs` — remplacement de `(price / tick).round() * tick` par `price.round_dp(dp)` avec `dp` calculé depuis `floor_log10(price)`.

### Doctest cassé dans `backtest/runner.rs`
- **Cause** : le bloc ` ``` ` du doc comment de `run_from_files` contenait un chemin de fichier (`data/l2/{coin}/...`) que `cargo test` tentait de compiler comme du code Rust → erreur de syntaxe.
- **Fix** : annoté ` ```text ` pour indiquer que le bloc est du texte littéral, pas du code exécutable.
- **Fichier** : `src/backtest/runner.rs`

## 2026-04-01 — Observability & Dry-Run Simulation

Après ~1h07 d'exécution en dry-run (683 signaux, 0 fills, 0 trades), ces lacunes ont été identifiées et corrigées :

### Journal wired to events
- `Journal` est maintenant thread-safe (`Mutex`) et branché sur tous les événements : OrderPlaced, OrderFilled, OrderCancelled, PositionOpened, PositionClosed, RiskRejection
- Méthode `log_event()` ajoutée (best-effort, log warning on failure)
- Suppression de la variable `_journal` — le journal est utilisé activement

### Dry-run fill simulator
- En mode DryRun, le mid price est comparé aux ordres pending :
  - Long : fill quand mid ≤ entry price (bid passif touché)
  - Short : fill quand mid ≥ entry price (ask passif touché)
- Simulation complète de sorties SL/TP (mid vs stops)
- Les positions simulées sont trackées, P&L calculé, equity mise à jour
- Les trades fermés alimentent le dashboard et les stats

### Per-coin signal cooldown (30s)
- Après émission d'un signal pour un coin, 30s de cooldown avant de re-émettre
- Évite le spam de signaux identiques (avant: BTC émettait 89 fois le même signal)
- Réduit la charge de logs de ~10 signaux/s à ~1 toutes les 30s par coin

### DEBUG feature logging
- `evaluate()` émet un log `DEBUG` avec toutes les features individuelles pour chaque signal émis (OFI, micro-price, VAMP, aggression, depth, toxicité, spread, imbalance, vol_ratio, intensity)
- Activable via `RUST_LOG=gbot::strategy::mfdp=debug`

### Periodic summary (every 5 minutes)
- Log `[SUMMARY]` émis toutes les 5 min avec : uptime, equity, positions ouvertes, trades fermés, win rate, P&L total, signaux/ordres/rejections/fills depuis le dernier résumé

### Signal persistence (`data/signals/`)
- Nouveau module `persistence::signal_recorder` — écrit chaque signal en JSONL avec le contexte complet : scores, features, prix entry/SL/TP, action (placed/risk_rejected), raison de rejet
- Un fichier par jour : `data/signals/YYYY-MM-DD.jsonl`

### API change (`MfdpStrategy::evaluate`)
- `evaluate()` retourne maintenant `(Intent, f64, f64)` — l'intent + dir_score + queue_score
- `evaluate_with_reduced_size()` mis à jour de même
- `BacktestRunner` adapté

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
