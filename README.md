# gbot — Microstructure-First Directional Pullback Bot

Bot de trading algorithmique haute fréquence pour **Hyperliquid**, implémenté en Rust. Stratégie MFDP (Microstructure-First Directional Pullback) : détection de biais directionnel via les features de carnet d'ordres et flux, entrée passive sur pullback avec ordres ALO (Add Liquidity Only).

## Principes

- **Microstructure first** : le signal vient du carnet L2 et du flux de trades, pas d'indicateurs retardés
- **Entrée passive uniquement** : ordres ALO (maker) pour capturer le rebate et éviter le slippage
- **Gestion du risque absolue** : veto binaire du RiskManager, circuit breaker sur drawdown
- **Observation → Dry-run → Live** : 3 modes progressifs avant de risquer du capital
- **Zéro dépendance OpenSSL** : rustls partout (build sans `libssl-dev`)

## Architecture

```
┌──────────┐   WS    ┌──────────────┐
│Hyperliquid├────────►│  BookManager │──► OrderBook + TradeTape (par coin)
└──────────┘         └──────┬───────┘
                            │
                    ┌───────▼───────┐
                    │ FeatureEngine │──► BookFeatures + FlowFeatures
                    └───────┬───────┘
                            │
                    ┌───────▼───────┐
                    │ RegimeEngine  │──► 8 régimes de marché
                    └───────┬───────┘
                            │
                    ┌───────▼───────┐
                    │ MfdpStrategy  │──► Intent (PlacePassiveEntry, ForceExitIOC, etc.)
                    └───────┬───────┘
                            │
                    ┌───────▼───────┐
                    │  RiskManager  │──► Veto / Pass (8 règles trade + 7 portfolio)
                    └───────┬───────┘
                            │
                    ┌───────▼───────┐
                    │ OrderManager  │──► 10-state machine (Flat → Filled → BreakEven → …)
                    │ PositionMgr   │──► Break-even, trailing stop, sync exchange
                    └───────┬───────┘
                            │
              ┌─────────────┼─────────────┐
              ▼             ▼             ▼
         Journal       Prometheus      Dashboard
         (JSONL)       (metrics)     (Axum HTTP)
```

## Modules

| Module | Fichiers | Description |
|--------|----------|-------------|
| `config` | `settings.rs`, `coins.rs` | Configuration TOML + métadonnées coins (tick/lot size, leverage) |
| `exchange` | `ws_client.rs`, `rest_client.rs`, `signer.rs`, `rate_limiter.rs` | WebSocket L2/trades, REST API, signature EIP-712, rate limiter token-bucket |
| `market_data` | `book.rs`, `book_manager.rs`, `recorder.rs` | OrderBook (BTreeMap), gestion multi-coins, enregistrement JSONL |
| `features` | `book_features.rs`, `flow_features.rs`, `engine.rs` | Features instantanées (spread, imbalance, VAMP) + temporelles (OFI, toxicité, vol) |
| `regime` | `engine.rs` | Classification en 8 régimes : QuietTight → DoNotTrade |
| `strategy` | `signal.rs`, `mfdp.rs` | Direction score (6 features pondérées), queue desirability, pullback detection |
| `execution` | `order_manager.rs`, `position_manager.rs` | Machine à 10 états, lifecycle position (BE, trailing, sync exchange) |
| `risk` | `manager.rs` | Veto absolu : 8 règles par trade, 7 règles portfolio, drawdown throttle/circuit breaker |
| `portfolio` | `state.rs` | État interne : positions, PnL réalisé, fees, funding, marge |
| `persistence` | `journal.rs`, `parquet_writer.rs` | Journal JSONL structuré, conversion Arrow/Parquet (`convert_book_jsonl`, `convert_trade_jsonl`) |
| `observability` | `metrics.rs`, `dashboard.rs` | 12 métriques Prometheus, dashboard HTTP (SSE temps réel, snapshot JSON) |
| `backtest` | `replay_engine.rs`, `runner.rs`, `sim_book.rs`, `sim_execution.rs` | Replay de données JSONL enregistrées à travers le pipeline complet (features → régime → stratégie), fill probabiliste |

## Features de scoring

### Book Features (instantanées)
- Spread (bps) et ratio vs moyenne mobile
- Imbalance top-1 / top-3 / top-5 / pondérée (5 niveaux)
- Profondeur bid/ask à 10 bps, depth ratio
- Micro-price et déviation vs mid (bps)
- VAMP (Volume-Adjusted Mid Price) et signal VAMP (bps)

### Flow Features (fenêtres glissantes)
- OFI (Order Flow Imbalance) à 1s, 3s, 10s, 30s
- Intensité de trades, taille moyenne, ratio gros trades
- Persistence d'agression (séries consécutives)
- Volatilité réalisée à 3s, 10s, 30s + ratio court/long
- Toxicité instantanée (proxy), vitesse de refill, ratio cancel/add (fenêtre glissante 60s)

## Régimes de marché

| Régime | Condition | Action |
|--------|-----------|--------|
| QuietTight | Spread serré, volume faible, vol basse | ✅ Trade (idéal) |
| QuietThin | Spread serré mais book peu profond | ⚠️ Taille réduite |
| ActiveHealthy | Spread OK, volume élevé, vol modérée | ✅ Trade |
| ActiveToxic | Volume élevé, haute toxicité | ⛔ Pas de nouvelles entrées |
| WideSpread | Spread > seuil max | ⛔ Coûts d'entrée trop élevés |
| NewslikeChaos | Vol ratio > 3x, spread variable | ⛔ Trop d'incertitude |
| LowSignal | Features insuffisantes/stale | ⚠️ Attendre |
| DoNotTrade | Catch-all pour conditions inconnues | ⛔ Interdiction absolue |

## Prérequis

- **Rust** ≥ 1.77 (stable)
- Pas besoin d'OpenSSL (rustls utilisé partout)

```bash
# Installer Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

## Configuration

Le fichier principal est `config/default.toml`. Toute valeur peut être surchargée par variable d'environnement avec le préfixe `GBOT__` (double underscore pour la hiérarchie) :

```bash
# Exemples de surcharges
export GBOT__GENERAL__MODE=dry_run
export GBOT__EXCHANGE__PRIVATE_KEY=0xabc...
export GBOT__RISK__MAX_OPEN_POSITIONS=3
```

### Sections de configuration

| Section | Description |
|---------|-------------|
| `general` | Mode d'exécution (`observation`, `dry_run`, `live`) |
| `exchange` | URLs WebSocket/REST, clé privée, wallet, rate limits, timeouts, reconnect |
| `coins` | Liste des coins tradés avec tick_size, lot_size, max_leverage, asset_index |
| `features` | Fenêtres OFI, taille du tape, moyenne de spread |
| `regime` | Seuils pour chaque régime (spread, volume, vol, toxicité) |
| `strategy` | Poids du direction score, seuils pullback, queue desirability |
| `risk` | Pertes max, positions max, drawdown throttle/circuit breaker, leverage |
| `execution` | Durée max position, MAE max, break-even, trailing stop |
| `recording` | Activation et intervalle de flush des données JSONL |

## Utilisation

### Build

```bash
cargo build --release
```

### Lancement

```bash
# Mode observation (défaut) — collecte de données, pas d'ordres
cargo run --release

# Mode dry-run via variable d'environnement
GBOT__GENERAL__MODE=dry_run cargo run --release

# Mode live (nécessite private_key et wallet_address)
GBOT__GENERAL__MODE=live \
GBOT__EXCHANGE__PRIVATE_KEY=0x... \
GBOT__EXCHANGE__WALLET_ADDRESS=0x... \
cargo run --release
```

### Docker

```bash
docker build -t gbot .
docker run -d --name gbot \
  -e GBOT__GENERAL__MODE=observation \
  -p 3000:3000 \
  -v $(pwd)/data:/app/data \
  gbot
```

### Endpoints HTTP

| Endpoint | Description |
|----------|-------------|
| `GET /` | Dashboard UI (single page HTML/JS/CSS) |
| `GET /api/state` | Snapshot JSON complet (positions, books, métriques, events) |
| `GET /api/stream` | SSE temps réel — push toutes les 500ms |
| `GET /health` | Health check |
| `GET /metrics` | Métriques Prometheus (text format) |

### Dashboard UI

Single page verticale sans onglets, 4 zones toujours visibles :

1. **Header** — Equity, Daily P&L, Drawdown, nombre de positions, indicateur live (vert/rouge/gris)
2. **Carnet en temps réel** (gauche) — Par coin actif : spread, imbalance (barre [-1,+1]), micro-price, toxicité (jauge), régime (badge coloré), éligibilité ALO
3. **Positions & Ordres** (droite) — Positions ouvertes avec P&L live, SL/TP, break-even, elapsed + ordres pending avec timer
4. **Métriques session** — Fill rate, adverse selection, spread capture, queue lag, reconnects, kill-switch
5. **Feed d'événements** — 30 derniers événements (fills, changements de régime, rejets risk, reconnects) colorés par type

Stack : HTML + JS vanilla + CSS custom (dark theme). Pas de dépendance externe, pas de CDN. SSE unique, pas de polling.

Accès : `http://localhost:3000` (local) ou via tunnel SSH (`ssh -L 3000:127.0.0.1:3000 gbot`). Voir `docs/deployment.md`.

### Déploiement serveur (Hetzner)

```bash
# Préparer le serveur (une seule fois)
./prepareServer.sh 46.224.43.198

# Déployer et démarrer
./deploy.sh --start

# Accéder à l'UI via tunnel SSH
ssh -L 3000:127.0.0.1:3000 gbot
# → http://localhost:3000
```

Voir `docs/deployment.md` pour le détail.

## Gestion du risque

### Par trade
- Perte max par trade : 1.5% de l'equity
- Slippage max : 0.5%
- Spread min/max : 0–10 bps
- Profondeur min : $5,000 USD
- Toxicité max : 0.7
- Ratio volatilité max : 3.0x

### Portfolio
- Positions simultanées max : 5
- Biais directionnel max : 3 (net long/short)
- Usage marge max : 60%
- Perte journalière max : 10%
- Drawdown throttle : 7% → positions /2, 12% → 1 position max
- Circuit breaker : 20% → kill-switch (arrêt total)
- Equity spike guard : ignore les sauts > 5% (protection contre données erronées)

### Break-even & Trailing
- Break-even déclenché à 50% de progression vers le TP
- Trailing stop tier 1 : à 65% de progression, lock 25% des gains
- Trailing stop tier 2 : à 80% de progression, lock 50% des gains

## Phases de développement

| Phase | Description | Statut |
|-------|-------------|--------|
| 0 | Scaffold complet, build propre | ✅ |
| 1 | Connectivité exchange, collecte de données | ✅ |
| 2 | Features live, régime classification, Parquet writer | ✅ |
| 3 | Strategy MFDP V1, sizing pipeline, SL/TP triggers | ✅ |
| 4 | Position lifecycle complet (BE, trailing, sync, recovery) | ✅ |
| 5 | Backtest sur données enregistrées (`BacktestRunner`) | ✅ |
| 6 | Dry-run / Live trading (taille minimale) | 🔲 |
| 7 | UI de monitoring (dashboard SSE + métriques) | ✅ |

## Protections intégrées (leçons de t-bot)

1. **Rate limiter fidèle** : poids réels par endpoint (pas de constantes approximatives)
2. **Timeout HTTP** : connect/read timeout sur toutes les requêtes REST (pas de hang infini)
3. **Exception propagée** : `get_open_positions()` propage l'erreur, ne retourne jamais une liste vide
4. **Safety guard sync** : si exchange retourne 0 positions mais tracking > 0, on ne ferme pas tout
5. **Equity pour drawdown** : equity totale (pas balance disponible après marge)
6. **Equity spike guard** : ignore les sauts > 5% en un cycle (protection données erronées)
7. **SL mis à jour sur exchange** : break-even fait cancel + replace du trigger order via `update_sl_trigger()`
8. **Position recovery** : récupération automatique des positions orphelines au restart et à chaque sync
9. **SL/TP triggers systématiques** : chaque entrée confirmée place immédiatement des trigger orders SL et TP
10. **Asset index par coin** : chaque ordre utilise l'index numérique Hyperliquid du coin (pas hardcodé à 0)
11. **Ajustement SL/TP sur fill drift** : si le fill dévie > 0.5% du prix du signal, SL/TP sont recalculés proportionnellement
12. **Funding boundary** : blocage des nouvelles entrées si le funding expire dans < N secondes
13. **Cancel/add ratio** : suivi rolling 60s des deltas de book pour détection de spoofing
14. **Reconnect avec backoff** : WebSocket reconnect exponentiel avec jitter
15. **Kill-switch** : arrêt total des entrées si drawdown > seuil critique

## Stack technique

| Composant | Crate | Version |
|-----------|-------|---------|
| Runtime async | tokio | 1.x |
| WebSocket | tokio-tungstenite | 0.24 (rustls) |
| HTTP client | reqwest | 0.12 (rustls) |
| Sérialisation | serde + serde_json | 1.x |
| Arithmétique décimale | rust_decimal | 1.x |
| Signature ECDSA | k256 + sha3 | 0.13 / 0.10 |
| Maps concurrentes | dashmap | 6.x |
| Configuration | config | 0.14 (TOML) |
| Métriques | prometheus | 0.13 |
| Dashboard HTTP | axum + tower-http (fs) | 0.7 / 0.5 |
| SSE streaming | tokio-stream + futures-util | 0.1 / 0.3 |
| Tracing | tracing + tracing-subscriber | 0.1 / 0.3 |
| Stockage columnar | arrow + parquet | 53 |

## Licence

Propriétaire — usage personnel uniquement.