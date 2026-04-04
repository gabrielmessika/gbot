# Changelog

## 2026-04-03 18h — min_direction_confirmations 5 → 3

**Problème** : 0 trades en 6h+ de run (Session 5 Phase B). 323 signaux générés, 1010 incréments de confirmation loggés, **aucun n'atteint 5/5**. Le signal s'inverse avant que 5 confirmations consécutives ne s'accumulent.

**Preuve** : Session 5 Phase A (overnight, même marché, même jour) avec `min_confirmations=3` → 46 trades en 7h, WR=37%, P&L=-$0.32/trade (quasi-neutre). Phase B avec `min_confirmations=5` → 0 trades.

**Fix** : `config/default.toml` — `min_direction_confirmations` : 5 → **3**.

Le filtre `trending_min_bps=5.0` reste inchangé (justifié empiriquement). Le vrai blocage était le combo confirmations=5 + pullback move timeout, pas le filtre trending.

---

## 2026-04-03 08h — Fix WS reconnect loop + backoff bug

**Symptôme** : bot en boucle de reconnexion WS (210 erreurs en 8 min avec 31 coins, puis 30 erreurs en 2 min avec 15 coins). 0 trades, tous les coins en DoNotTrade.

### Cause 1 : trop de coins → IP rate limit
31 coins × 2 subs = 62 WS subscriptions → Hyperliquid coupe la connexion → les reconnects rapides déclenchent un ban IP temporaire → même avec 15 coins le ban persiste.

**Fix** : `config/default.toml` — réduit de 31 à **15 coins**.

### Cause 2 : backoff jamais appliqué (`src/exchange/ws_client.rs`)
**Bug** : `connect_and_listen()` retournait `Ok` (fermeture "propre") même quand la connexion durait <1s → le backoff était reset à `initial_delay_ms` à chaque cycle → le bot spammait les reconnects à 1s d'intervalle indéfiniment.

**Fix** : le backoff n'est reset que si la connexion a vécu >30s. Une connexion <30s est traitée comme un échec → le backoff continue de croître (5s, 10s, 20s, 40s, 60s max).

### Cause 3 : burst de subscriptions (`src/exchange/ws_client.rs`)
**Trouvé après la 2e itération** : le bot envoyait 30+ messages `subscribe` instantanément à la connexion. Hyperliquid coupe le WS après ~700ms quand il reçoit trop de messages en rafale (11 coins reçus sur 15 avant le reset).

**Fix** : ajout de `sleep(100ms)` entre les subscriptions de chaque coin. 15 coins × 100ms = 1.5s de subscription pacing au lieu de 0ms.

### Config reconnect
| Param | Avant | Après |
|-------|-------|-------|
| `initial_delay_ms` | 1000 | **5000** |
| `max_delay_ms` | 30000 | **60000** |

Avec backoff exponentiel : 5s → 10s → 20s → 40s → 60s. Au lieu de 2.5s × ∞.

---

## 2026-04-03 — V2.3 : TP sortie maker (changement structurel)

**Contexte** : relecture des plans originaux (plan_old1/2/3.md). Le plan prévoyait "TP passif | ALO limit" mais l'implémentation utilisait un trigger order (taker 4.5 bps). Round-trip fees = 6 bps au lieu des 3 bps prévus.

### Changement : TP = ALO limit reduce_only (maker)
- `open_position_with_triggers()` : TP placé via `rest.place_order()` avec `Tif::Alo, reduce_only: true` au lieu de `rest.place_trigger_order()`
- `close_position()` retourne `Option<String>` (TP OID à cancel) au lieu de `()`
- Nouveau `find_coin_by_tp_oid()` sur `PositionManager` pour matcher les fills TP ALO
- Handling complet du fill TP ALO dans le flux `orderUpdates` : cancel SL trigger, close position, record P&L avec maker fee
- `OpenPosition.tp_trigger_oid` renommé → `tp_order_oid`
- SL reste un trigger order (taker) — défensif, non-négociable

### Impact fees
| Scénario | Avant (trigger) | Après (ALO) |
|----------|----------------|-------------|
| Entry | maker 1.5 bps | maker 1.5 bps |
| TP exit | taker 4.5 bps | **maker 1.5 bps** |
| SL exit | taker 4.5 bps | taker 4.5 bps |
| Round-trip (TP) | **6.0 bps** | **3.0 bps** |
| Breakeven WR (RR=1.5) | ~55% | **~40%** |

### Fichiers
- `src/execution/position_manager.rs` — TP ALO placement, `find_coin_by_tp_oid()`, `close_position()` return type
- `src/main.rs` — TP ALO fill detection, cancel TP on close, cancel SL on TP fill

### 5.2 Stale quote management (`src/main.rs`)
- À chaque book update, si un coin a un TP ALO resting :
  - `toxicity > max_toxicity` → cancel TP ALO
  - Régime `ActiveToxic` ou `NewslikeChaos` → cancel TP ALO
- Le SL trigger reste actif, le max_hold fermera la position
- Évite les fills TP dans des conditions de marché adverses

### 5.3 Signal inverse exit (`src/main.rs`)
- À chaque book update, pour les coins en position :
  - Calcul rapide `pr5s_norm = sign(pr5s) × min(|pr5s|/5, 1)`
  - Long + `pr5s_norm < -0.5` → `ForceExitIoc` immédiat
  - Short + `pr5s_norm > +0.5` → `ForceExitIoc` immédiat
- Coupe les pertes quand le momentum s'inverse fortement
- Close reason: `"signal_inverse"`

### 5.5 Smart max hold (`src/main.rs`)
- À 70% du `max_hold_s` (~31s sur 45s), si le trade est en perte → sortie anticipée
- Les trades profitables continuent jusqu'au TP, trailing, ou max_hold normal
- Close reason: `"smart_exit_Ns"`
- Réduit l'exposition sur les perdants, libère le slot plus vite

### Diversification coins : +18 coins dont xyz dex

#### `src/exchange/rest_client.rs`
- Ajout `fetch_xyz_meta()` — récupère les métadonnées HIP-3 (`"type": "meta", "dex": "xyz"`)

#### `src/config/coins.rs`
- `from_exchange_meta()` refactoré → `add_universe(universe, is_dex)` privé
- `add_xyz_meta()` — ajoute les xyz avec `asset_index += 110_000`

#### `src/main.rs`
- Chargement séquentiel : standard perps puis xyz dex au startup

#### `src/market_data/book_manager.rs`
- Mid price calculé depuis le book (best_bid+best_ask/2) à chaque update
- Nécessaire car les xyz coins ne sont pas dans le flux WS `allMids`

#### `config/default.toml`
| Ajouté | Catégorie |
|--------|-----------|
| WIF, PEPE, TIA, SEI, INJ, JUP, PENDLE, W, ONDO | Mid-cap perps |
| TSLA, AAPL, NVDA, SPY, QQQ | xyz stocks |
| EUR, GBP, JPY | xyz forex |
| GOLD, SILVER | xyz commodities |

### Évolutions restantes
- ⏳ Stratégies supplémentaires — mean revert, imbalance fade, breakout flow (effort majeur)

---

## 2026-04-03 — V2.2 : Recalibration SL/TP + filtre trending renforcé

**Contexte** : V2.1 a empiré les résultats (WR 46.5% → 13.4%, PF 0.80 → 0.10).
Cause : SL=5bps dans le bruit (mouvement moyen 2.9bps), fees round-trip (6bps) > SL distance.

### R1 — SL floor : 5 → 8 bps (`config/default.toml`)
- SL=5bps = 1.7σ du bruit → 51% SL hit rate (vs 24% à 12bps en V2.0)
- Règle : **SL ≥ 2× round-trip fees (6bps)**. SL=8bps = safe zone.
- `sl_min_bps = 8.0`

### R2 — RR ratio : 2.0 → 1.5, TP ~12bps (`config/default.toml`)
- RR=2.0 exigeait WR ≥ 67% (irréaliste). RR=1.5 → breakeven WR=40%.
- `target_rr = 1.5`

### R3 — Filtre trending : 3.0 → 5.0 bps (`config/default.toml`)
- V2.1 : 82 trades passaient malgré RangingMarket (seuil trop bas).
- Avec fees=6bps, un marché qui bouge <5bps/30s n'a pas d'edge exploitable.
- `trending_min_bps = 5.0`

### R4 — Confirmations : 3 → 5 (`config/default.toml`)
- 8.2 trades/h trop fréquent. Objectif : 2-4 trades/h de haute qualité.
- `min_direction_confirmations = 5`

### Trailing/BE réalignés avec SL=8/TP=12
- `breakeven.trigger_pct = 50%` (50% de 12bps = 6bps = couvre les fees)
- `trailing.tier1 = 60%/30%`, `tier2 = 80%/50%`
- `max_mae_bps = 12.0`

---

## 2026-04-02 21h — Fix fee accounting dry-run + live exit taker fee

### Dry-run : double/triple-comptage des fees (P&L faussé)
- **Bug** : à la sortie dry-run, `entry_fee` était déduite du `pnl_usd` (net) ET re-passée dans `portfolio.record_fee(total_fees)`. Comme `portfolio.net_pnl() = realized_pnl - fees`, les fees étaient comptées 3× : 1× à l'entrée, 1× déduite du P&L, 1× dans record_fee à la sortie.
- **Fix** : aligné sur le chemin live — `record_pnl(gross)` + `record_fee(exit_fee_only)`. L'equity simulée utilise `net_pnl = gross - entry_fee - exit_fee` pour la progression réelle.
- **Impact** : les P&L dry-run étaient ~0.03% trop pessimistes par trade (entry fee comptée 2× de trop).

### Live : exit fee toujours maker → taker pour SL
- **Bug** : à la sortie live, le fee rate était hardcodé à 0.015% (maker) quelle que soit la raison de sortie. Les SL sont des trigger orders qui s'exécutent en market (taker = 0.045%).
- **Fix** : `exit_fee_rate = 0.045%` si `reason.contains("SL")`, `0.015%` sinon.
- **Impact** : les fees de sortie SL étaient sous-estimées de 3× (0.015% vs 0.045%).

### Fichiers
- `src/main.rs` — 2 blocs modifiés (sortie live l.882, sortie dry-run l.1065)

---

## 2026-04-02 20h35 — V2.1 : Fix RangingMarket + SL/TP calibration + trailing

**Contexte** : analyse session V2 (4.4h, 172 trades). WR=46.5%, P&L=-$82.
Cause racine : 0 TP hit après les 36 premières minutes (marché flat, SL/TP inatteignables).
- 11 TP (+$121) tous dans Q1 (trending) vs 32 SL réels (-$239) répartis uniformément
- 120/172 trades (70%) finissent en max_hold avec P&L≈$0 (mouvement moyen 4.6bps, TP à 18bps)
- RangingMarket ne se déclenchait jamais (0 occurrences en 4.4h — bug de placement)

### R1 — Fix bug RangingMarket (`src/regime/engine.rs`)
- **Bug** : le check `|price_return_30s| < trending_min_bps` était placé APRÈS QuietTight et ActiveHealthy dans `classify()`. Un marché flat avec spread serré = QuietTight avant d'atteindre le check → 0 déclenchements.
- **Fix** : déplacé le check RangingMarket **AVANT** les régimes tradables (QuietTight, QuietThin, ActiveHealthy), juste après WideSpread. Suppression de l'ancien check en fin de cascade.
- **Impact attendu** : élimine les trades en marché plat (~60% du volume Q2-Q4).

### R2 — Réduction SL/TP (`config/default.toml`)
| Param | Avant | Après | Raison |
|-------|-------|-------|--------|
| `sl_min_bps` | 12.0 | **5.0** | Mouvement moyen 4.6bps/46s → SL=12 rarement touché |
| `sl_max_bps` | 80.0 | **30.0** | Cap trop large inutile |
| `sl_vol_multiplier` | 4.0 | **3.0** | Réduction overshoot |
| `target_rr` | 1.5 | **2.0** | TP~10bps atteignable. Breakeven WR=33% (était 40%) |
| `max_mae_bps` | 15.0 | **8.0** | Aligné avec SL plus serré |

### R3 — Thresholds plus exigeants (`config/default.toml`)
| Param | Avant | Après | Raison |
|-------|-------|-------|--------|
| `direction_threshold_long` | 0.52 | **0.60** | Dir score moyen=0.32, signaux tièdes éliminés |
| `direction_threshold_short` | -0.52 | **-0.60** | Idem |

### R4 — Trailing stop plus agressif (`config/default.toml`)
| Param | Avant | Après | Raison |
|-------|-------|-------|--------|
| `breakeven.trigger_pct` | 50.0 | **40.0** | BE plus tôt avec TP plus proches |
| `trailing.tier1_progress_pct` | 65.0 | **50.0** | Ancien tier jamais atteint (TP=18bps → 65%=12bps) |
| `trailing.tier1_lock_pct` | 25.0 | **30.0** | Lock 30% profit à 50% progress |
| `trailing.tier2_progress_pct` | 80.0 | **70.0** | Accessible avec TP=10bps |

---

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
  - ~~Placement initial après LowSignal~~ **Bug V2.0** : ne se déclenchait jamais car QuietTight matchait en premier. Corrigé en V2.1 (déplacé avant les régimes tradables).
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
