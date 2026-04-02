# Changelog

## 2026-04-02 16h02 — Pivot signal + regime filter refactor

### Pivot signal : OFI → Price Momentum (empiriquement prouvé)

**Contexte** : analyse des données du serveur (dry-run 2026-04-01/02) a révélé que l'OFI est du bruit sur Hyperliquid (corr=+0.058, WR=2.3% directionnel). Le prix momentum (pr5s) est le signal dominant (corr=+0.354, WR=68% en marché tendanciel).

#### `src/features/flow_features.rs`
- Ajout des champs `price_return_5s`, `price_return_10s`, `price_return_30s` dans `FlowFeatures`
- Ajout des appels `compute_price_return(tape, now_ms, N)` dans `compute_flow_features()`
- Ajout de la fonction `compute_price_return()` : retourne `(last_price - first_price) / first_price × 10_000` en bps, 0.0 si < 2 trades dans la fenêtre
- OFI conservé pour observabilité mais n'est plus utilisé dans le scoring directionnel

#### `src/strategy/mfdp.rs`
- **Refonte complète de `compute_direction_score()`** : remplace OFI + aggression par price momentum
  - Nouveau : `w_pr5s × sign(pr5s) × min(|pr5s|/5, 1) + w_pr10s × sign(pr10s) × min(|pr10s|/10, 1)`
  - Conservé : `w_micro_price × micro_norm + w_vamp × vamp_norm + w_depth_imb × depth_imbalance`
  - Penalité : `- w_toxicity × toxicity_penalty`
  - `depth_signed` (depth_ratio - 1) remplacé par `imbalance_weighted` (déjà signé, multi-niveau)
- Ajout de `Regime::RangingMarket` dans le gate de régime (no-trade)
- Log debug mis à jour : affiche `pr5s`, `pr10s`, `depth_imb` au lieu de `ofi_10s`, `aggression`
- `max_wait_pullback_s` → `pullback_wait_retrace_s`

#### `src/regime/engine.rs`
- Ajout du variant `RangingMarket` dans l'enum `Regime`
- Classification : si `|price_return_30s| < trending_min_bps` → `RangingMarket` (no-entry)
  - Placement après LowSignal dans la cascade pour que les filtres spread/vol s'appliquent d'abord
- `allows_entry()` : `RangingMarket` exclu (comme LowSignal)
- **Justification empirique** : WR directionnel = 0% quand |pr30s| < 3bps (marché plat)

#### `src/strategy/pullback.rs`
- **Correction bug majeur** : WaitingMove et WaitingPullback partageaient le même `expires_at`
  - Symptôme : 45/50 setups avortaient (WaitingMove consommait ~20s → WaitingPullback avait <10s)
  - Fix : `max_wait_ms` → `wait_move_ms` + `wait_retrace_ms` indépendants
  - À la transition WaitingMove → WaitingPullback : `expires_at` reset à `now_ms + wait_retrace_ms`

#### `src/config/settings.rs`
- `StrategySettings` : suppression `w_ofi`, `w_aggression`, `w_depth_ratio`, `max_wait_pullback_s`
- `StrategySettings` : ajout `w_pr5s`, `w_pr10s`, `w_depth_imb`, `pullback_wait_move_s`, `pullback_wait_retrace_s`
- `StrategySettings` : défauts mis à jour (`sl_min_bps=12.0`, `sl_vol_multiplier=4.0`, `target_rr=1.5`, `pullback_min_move_bps=1.5`)
- `RegimeSettings` : ajout `trending_min_bps` (défaut 3.0 bps), avec `impl RegimeSettings::default_trending_min_bps()`
- `RiskSettings` : ajout `max_signals_per_coin_10min` (défaut 6), avec `impl RiskSettings::default_max_signals_per_coin_10min()`

#### `src/main.rs`
- Construction de `PullbackSettings` : `max_wait_ms` → `wait_move_ms` + `wait_retrace_ms`
- Ajout quota signal par coin (fenêtre glissante 10min) : `coin_signal_timestamps: HashMap<String, VecDeque<i64>>`
  - Avant chaque émission de signal : purge des timestamps > 10min, vérification quota
  - Si `ts_queue.len() >= max_signals_per_coin_10min` → skip avec log debug
  - **Justification** : ETH reçoit ~10× plus de BookUpdates → monopolisait les signaux (5 trades en 2h)

#### `config/default.toml`
| Paramètre | Avant | Après | Raison |
|-----------|-------|-------|--------|
| `max_hold_s` | 600 | 45 | Autocorrélation momentum → 0 à 60s |
| `sl_min_bps` | 8.0 | 12.0 | Floor trop bas → SL sur le bruit |
| `pullback_min_move_bps` | 3.0 | 1.5 | Seuil trop élevé → peu de setups |
| `pullback_wait_move_s` | — | 20 | Phase WaitingMove indépendante |
| `pullback_wait_retrace_s` | — | 20 | Phase WaitingPullback indépendante |
| `max_wait_pullback_s` | 30 | supprimé | Remplacé par les deux ci-dessus |
| `w_ofi` | 0.25 | supprimé | corr=+0.058 → bruit |
| `w_aggression` | 0.15 | supprimé | Colinéaire à OFI (corr=0.999) |
| `w_depth_ratio` | 0.10 | supprimé | Remplacé par `w_depth_imb` |
| `w_pr5s` | — | 0.40 | Signal primaire (corr=+0.354) |
| `w_pr10s` | — | 0.20 | Momentum secondaire |
| `w_depth_imb` | — | 0.15 | Profondeur multi-niveau signée |
| `trending_min_bps` | — | 3.0 | Filtre marché plat |
| `max_signals_per_coin_10min` | — | 6 | Quota anti-monopole |

#### `src/market_data/recorder.rs` (session précédente)
- Ajout `bid_levels: Vec<[f64; 2]>` et `ask_levels: Vec<[f64; 2]>` dans `BookRecord`
- `record_book()` : population via `book.top_bids(10)` et `book.top_asks(10)`
- Fichiers L2 ~3× plus volumineux — nécessaire pour valider le multi-level OBI

#### `research/scripts/analyze_obi_levels.py` (nouveau)
- Script d'analyse OBI multi-niveau (L1 à L10) sur les données JSONL du recorder
- Calcule `corr(obi_lN, ret_30s)` et WR directionnel pour chaque profondeur
- **Critère de décision** : OBI_LN doit avoir `|corr| >= 2× |corr_L1|` pour justifier l'implémentation
- Usage : `python analyze_obi_levels.py --data-dir ./data/l2 --coin ETH`

## 2026-04-02 — P0 bug fix + config tuning post dry-run analysis

Analysis of the 2026-04-01/02 dry-run session (28 trades, WR=39%, P&L=-$90.38) revealed a critical bug and several config issues. The bot was stuck in DoNotTrade for 16h+ after a WS reconnect.

### P0 — BUG: DoNotTrade permanent after WebSocket reconnect (critical)

### Risk state persistence across restarts
- **Problem**: After restart, `peak_equity`, `daily_start_balance`, `daily_reset_ts` and `kill_switch` were reset. A -8% drawdown before restart + -8% after = -16% real but undetected by circuit breaker.
- **Fix**: `RiskManager::new_with_persistence()` saves/loads state from `data/risk_state.json`.
  - State saved every 5 minutes (summary cycle) + on graceful shutdown (SIGINT/ctrl-c) + on event channel close.
  - On startup: loaded if < 24h old, otherwise starts fresh.
  - Persisted fields: `peak_equity`, `daily_start_balance`, `daily_reset_ts`, `kill_switch_active`.
  - **Live priority**: exchange data is always authoritative. `peak_equity = max(exchange_equity, persisted_peak)` — never artificially lowered. `daily_start_balance`/`daily_reset_ts`/`kill_switch` come from persistence only (not available from exchange).
  - `check_daily_reset()` runs immediately at startup — if bot restarts after midnight UTC, daily counters reset on actual exchange equity.
- **Graceful shutdown**: Added `tokio::signal::ctrl_c()` handler in the main select loop.
- **Files**: `src/risk/manager.rs`, `src/main.rs`

### P0 — BUG: DoNotTrade permanent after WebSocket reconnect (critical)
- **Symptom**: After a WS reconnect at 19:49 UTC, all 12 coins switched to `DoNotTrade` and never recovered. 0 signals for 16h+.
- **Root cause**: On `Reconnected`, `book_stale` was set to `true` for all coins, but `OrderBook::snapshot_loaded` was NOT reset. The next l2Book messages were treated as deltas (not snapshots), and `apply_delta()` never clears `book_stale`. So `is_stale()` returned `true` forever → regime always `DoNotTrade`.
- **Fix**: Reset `snapshot_loaded = false` for all books on `Reconnected`, so the next l2Book is treated as a fresh snapshot which properly clears `book_stale`.
- **File**: `src/market_data/book_manager.rs`

### P1 — SL/TP tuning (SL floor too tight, TP unreachable)
- SL was hardcoded at 15 bps floor in practice (vol 30s always below floor). TP at 30 bps never hit in 5 min max_hold. Winners came from timeout, not TP.
- `sl_vol_multiplier`: 2.5 → **4.0** (scale better with real vol)
- `sl_min_bps`: 15.0 → **8.0** (let vol multiplier operate)
- `target_rr`: 2.0 → **1.5** (achievable TP within hold time)
- `max_hold_s`: 300 → **600** (10 min — more time for TP to realize)
- **File**: `config/default.toml`

### P2 — ETH over-representation (53% of signals)
- 75/143 signals were ETH Long. 16 ETH trades in 2h46 due to short cooldowns.
- `signal_cooldown_ms`: 30s → **60s** (`src/main.rs`)
- `cooldown_after_close_s`: 60 → **120** (`config/default.toml`)

### P3 — Entry quality filters too loose
- Most signals had dir_score barely above 0.50 (noise entries). NEAR SL_HIT in 11s, 7 consecutive SL_HIT streak.
- `direction_threshold_long`: 0.5 → **0.52**
- `direction_threshold_short`: -0.5 → **-0.52**
- `queue_score_threshold`: 0.3 → **0.5** (observed min was 0.52)
- **File**: `config/default.toml`

### P4 — WS reconnect frequency
- 6 reconnects in 17h. `ws_stale_s=60` too aggressive (normal network delay > 60s triggers reconnect).
- `ws_stale_s`: 60 → **120**
- **File**: `config/default.toml`

## 2026-04-01 — Fix force exit spam bug

### Bug: ForceExitIoc fired every tick after max_hold timeout

- **Symptom**: Once `max_hold_s` was exceeded, `[ORDER] Force exit` logged every ~1s (60+ lines) but position not closed in dry-run
- **Root cause**: 3 issues combined:
  1. `main.rs` max_hold check had no guard against state already being `ForceExit` → re-sent intent every tick
  2. `ForceExitIoc` handler in `order_manager.rs` had no duplicate check → re-processed every time
  3. Dry-run exit simulation only checked SL/TP hits, never handled `ForceExit` state → position never closed by force exit
  4. After dry-run exit, `order_mgr` state was not reset to `Flat`

### Fixes:
- **order_manager.rs**: Early return in `ForceExitIoc` if state is already `ForceExit`
- **order_manager.rs**: Added `set_flat()` public method for post-close state reset
- **main.rs**: Guard max_hold and regime-forced exit blocks with `!matches!(state, ForceExit { .. })`
- **main.rs**: Dry-run exit simulation now handles `ForceExit` state → closes position at current mid
- **main.rs**: After any dry-run close, resets `order_mgr` state to `Flat`

## 2026-04-01 — Backtest sizing réel + binary CLI (`src/bin/backtest.rs`)

### Limitation supprimée : taille de position fixe 0.001 coin

- **Avant** : taille hardcodée à `0.001 coin` → P&L en unités arbitraires, non comparables
- **Après** : `RiskManager::compute_position_size()` utilisé à chaque trade
  - `size_usd = equity × max_loss_per_trade_pct / sl_distance_pct`
  - Capé par `max_leverage` (config) et `max_margin_usage_pct`
  - Equity mise à jour après chaque trade fermé (trades suivants sized sur P&L réel)
- `BacktestTrade` enrichi : `size_usd` (notional en $) + `leverage` (levier effectif utilisé)
- `BacktestRunner::new()` prend maintenant `&Settings` pour accéder aux paramètres de risque

### Binary CLI `cargo run --bin backtest`

Commandes :
```
cargo run --bin backtest -- --date 2026-04-01
cargo run --bin backtest -- --date 2026-04-01 --compare 30
cargo run --bin backtest -- --coins BTC,ETH,SOL --equity 5000
```
Auto-détection des coins disponibles pour la date demandée. Affiche breakdown par coin avec `size_usd`, `leverage`, `adverse_5s%`, `MAE/MFE`.

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
