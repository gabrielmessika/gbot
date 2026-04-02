# gbot — Plan de développement final

> **Bot directionnel maker-first, queue-aware, microstructure-driven** pour Hyperliquid perps.
> Nom de travail : **MFDP** (Microstructure First Directional Pullback).
>
> **Philosophie** : le signal vient du carnet. L'exécution cherche d'abord à être maker.
> Le taker n'est utilisé qu'en défense ou quand l'avantage statistique est exceptionnellement fort.
>
> **Ce bot n'est PAS un market maker** (pas de bid+ask simultanés, pas de gestion d'inventaire bilatérale).
> C'est du scalping directionnel piloté par les données L2, avec entrée maker (ALO) pour minimiser les frais.

---

## Table des matières

1. [Pourquoi ce bot](#1-pourquoi-ce-bot)
2. [Contraintes fondamentales Hyperliquid](#2-contraintes-fondamentales-hyperliquid)
3. [Stack technologique](#3-stack-technologique)
4. [Architecture globale](#4-architecture-globale)
5. [Modules détaillés](#5-modules-détaillés)
6. [Features microstructure](#6-features-microstructure)
7. [Regime engine](#7-regime-engine)
8. [Stratégie MFDP V1](#8-stratégie-mfdp-v1)
9. [State machine de trading](#9-state-machine-de-trading)
10. [Exécution](#10-exécution)
11. [Risk management](#11-risk-management)
12. [Position lifecycle](#12-position-lifecycle)
13. [Backtest et replay](#13-backtest-et-replay)
14. [Stockage, observabilité et UI](#14-stockage-observabilité-et-ui)
15. [Sécurité opérationnelle](#15-sécurité-opérationnelle)
16. [Pipeline de recherche](#16-pipeline-de-recherche)
17. [Ordre de développement](#17-ordre-de-développement)
18. [Leçons des bugs t-bot / tbot-scalp](#18-leçons-des-bugs-t-bot--tbot-scalp)
19. [Checklist pré-live](#19-checklist-pré-live)

---

## 1. Pourquoi ce bot

tbot-scalp est un bot chandelier (OHLCV → indicateurs lissés → signal → exécution). Sur 1m/3m, cette approche est structurellement handicapée :

- **Signaux retardataires** : EMA, RSI, Bollinger sont calculés sur des bougies fermées → le signal arrive après que le move a commencé
- **Pas de vision du carnet** : pas de spread, pas d'imbalance, pas de depth → impossible de distinguer un breakout sain d'un fakeout dans un book fin
- **Exécution aveugle** : le bot sait quand entrer mais pas à quel prix le book peut absorber → slippage imprévisible
- **Backtest trompeur** : les backtests OHLCV surestiment fortement la rentabilité du scalp (frais mal modélisés, slippage dynamique absent)

Ce bot inverse la logique : **le signal vient du carnet, l'exécution est guidée par le carnet**.

### Ce que le bot fera (V1)

- Écouter le WebSocket Hyperliquid en continu
- Maintenir un état local temps réel du marché (book, trades, features)
- Classer le contexte de marché par régime avant toute décision
- Détecter un biais directionnel court terme via les features microstructure
- Attendre un micro-pullback local pour entrer en maker (ALO)
- Gérer les sorties par règles explicites (passif ou défensif)
- Journaliser tout pour audit, replay et analyse offline

### Ce que le bot ne fera PAS (V1)

- Deep learning en ligne ou RL
- Arbitrage multi-exchange
- Market making symétrique permanent
- Trading sur plus de 1-3 actifs simultanément
- Hedging spot/perp
- Cross-venue smart order routing

---

## 2. Contraintes fondamentales Hyperliquid

### 2.1. Frais

| Mode | Taux | Aller-retour |
|------|------|-------------|
| Taker → Taker | 0.045% | **~9 bps** |
| Maker → Taker | 0.015% + 0.045% | **~6 bps** |
| Maker → Maker | 0.015% + 0.015% | **~3 bps** |

Conséquence directe : un bot qui entre et sort au marché doit générer un edge brut ≥ 9 bps juste pour couvrir les frais, avant slippage. La stratégie doit minimiser les sorties taker.

### 2.2. Levier limité par actif

Le levier max varie par actif (BTC: 40-50x, SOL: 20x, small caps: 5-10x). Un edge de quelques bps devient insignifiant avec un levier faible si les frais ne sont pas maîtrisés. Le bot **sélectionne les marchés** selon leur liquidité, spread, profondeur et comportement du carnet.

### 2.3. Latence

Hyperliquid annonce ~200ms de latence end-to-end médiane. Python asyncio peut tenir cette contrainte, mais Rust offre une latence plus basse, plus stable, et une robustesse opérationnelle 24/7 supérieure. Choix retenu : **Rust pour le moteur live**.

### 2.4. TP/SL exchange

Les trigger orders Hyperliquid sont des market orders déclenchés au mark price, avec une tolérance de slippage de 10%. Pour un bot scalp très court terme, la **logique de sortie principale reste dans le moteur**, les triggers exchange sont des filets de sécurité, pas la stratégie de sortie primaire.

---

## 3. Stack technologique

### 3.1. Moteur live : Rust

| Raison | Détail |
|--------|--------|
| Latence et stabilité | Pas de GIL, pas de GC, latence prévisible |
| Robustesse 24/7 | Gestion d'erreurs explicite, types stricts, pas d'exceptions silencieuses |
| Concurrence | `tokio` async, pas de race conditions liées au GIL |
| Types numériques | Contrôle fin sur les représentations numériques (fixed-point pour les prix) |
| State machines | Pattern enum + match = state machine native, impossible d'oublier un état |

### 3.2. Dépendances Rust

```toml
[dependencies]
tokio            = { version = "1", features = ["full"] }
tokio-tungstenite = "0.21"        # WebSocket client
reqwest          = { version = "0.12", features = ["json"] }
serde            = { version = "1", features = ["derive"] }
serde_json       = "1"
rust_decimal     = "1"            # Fixed-point pour les prix (jamais f64 pour les ordres)
thiserror        = "1"
anyhow           = "1"
tracing          = "1"
tracing-subscriber = "1"
uuid             = { version = "1", features = ["v4"] }
parquet          = "51"           # Stockage columnar
arrow            = "51"
prometheus       = "0.13"
config           = "0.14"         # Parsing TOML/YAML
dashmap          = "5"            # Concurrent HashMap (pour le state store)
eth-abi-rs       = "..."          # EIP-712 signing — ou port du HyperliquidSigner Java
```

### 3.3. Recherche / analyse offline : Python

```
pandas / polars    — analyse des données historiques
numpy              — calculs vectoriels
duckdb             — requêtes SQL sur les fichiers Parquet
jupyter            — notebooks d'exploration
scipy / sklearn    — calibration des seuils, études de prédictibilité
matplotlib/plotly  — visualisation
```

### 3.4. Stockage

| Usage | Format | Outil |
|-------|--------|-------|
| Données L2 brutes | Parquet (par coin/jour) | arrow-rs |
| Features calculées | Parquet (par coin/jour) | arrow-rs |
| Événements ordres/fills | Parquet | arrow-rs |
| Journal JSONL (fallback/debug) | JSONL | serde_json |
| Analyse offline | Parquet | DuckDB |
| Évolution si volume croît | — | ClickHouse |

**Pourquoi Parquet vs JSONL** : compressé, columnar, requêtes 100× plus rapides sur DuckDB, compatible avec tout l'écosystème Python/Arrow.

### 3.5. Ce qu'on n'utilise PAS

| Outil | Raison |
|-------|--------|
| ccxt | Trop générique, cache la mécanique Hyperliquid (ALO, trigger orders, xyz dex) |
| Microservices | Latence réseau, complexité inutile en V1 |
| Base de données relationnelle | In-memory + Parquet suffisant |
| Python pour le moteur live | GIL, fragilité opérationnelle, latence imprévisible |
| Threading (Rust) | Préférer ownership claire + channels tokio → pas de `Arc<Mutex<>>` là où c'est évitable |

---

## 4. Architecture globale

### 4.1. Structure du projet

```
gbot/
├── src/
│   ├── main.rs
│   ├── config/
│   │   ├── settings.rs          — Config runtime (TOML/env)
│   │   └── coins.rs             — Coin metadata (asset index, tick, lot, max_leverage)
│   ├── exchange/
│   │   ├── ws_client.rs         — WebSocket persistant (reconnect, heartbeat)
│   │   ├── rest_client.rs       — REST async (/info, /exchange) + rate limiter
│   │   ├── signer.rs            — EIP-712 signing (port de HyperliquidSigner)
│   │   └── rate_limiter.rs      — Token bucket (1200 weight/min, poids réels)
│   ├── market_data/
│   │   ├── book.rs              — Order book local par coin
│   │   ├── book_manager.rs      — Reconstruction book depuis deltas WS
│   │   └── recorder.rs          — Enregistrement L2/trades bruts en Parquet
│   ├── features/
│   │   ├── book_features.rs     — Features instantanées (spread, imbalance, micro-price, VAMP)
│   │   ├── flow_features.rs     — Features temporelles (OFI, toxicity, vol réalisée)
│   │   └── engine.rs            — Orchestration calcul features
│   ├── regime/
│   │   └── engine.rs            — Classification du régime marché
│   ├── strategy/
│   │   ├── signal.rs            — Types Signal, Intent
│   │   └── mfdp.rs              — Stratégie MFDP V1
│   ├── execution/
│   │   ├── order_manager.rs     — Placement/annulation/amend, suivi pending
│   │   └── position_manager.rs  — Lifecycle positions (break-even, trailing, sync)
│   ├── risk/
│   │   └── manager.rs           — Validation pré-trade + portfolio risk
│   ├── portfolio/
│   │   └── state.rs             — Vérité interne du portefeuille
│   ├── persistence/
│   │   ├── journal.rs           — JSONL ordres (debug/audit)
│   │   └── parquet_writer.rs    — Écriture async Parquet
│   ├── observability/
│   │   ├── metrics.rs           — Prometheus
│   │   ├── dashboard.rs         — Axum : API REST + SSE stream
│   │   └── static/              — UI frontend (HTML/JS/CSS, servi par Axum)
│   └── backtest/
│       ├── replay_engine.rs     — Replay tick-by-tick depuis Parquet
│       ├── sim_book.rs          — Book simulé
│       └── sim_execution.rs     — Fill probability + queue model
├── research/                    — Python notebooks, calibration
│   ├── notebooks/
│   └── scripts/
├── config/
│   └── default.toml
├── Cargo.toml
├── Dockerfile
└── tests/
├── docs/
    └── deployment.md
    └── changelog.md
README.md
```

### 4.2. Flux de données

```
Hyperliquid WS ──┬── l2Book deltas  ──→ BookManager ──→ Features ──→ RegimeEngine
                 ├── trades          ──→ BookManager (tape + flow features)       |
                 ├── allMids         ──→ PriceCache                               |
                 └── orderUpdates    ──→ OrderManager (fills primaires)            |
                                                                                   v
                                                                            Strategy (MFDP)
                                                                                   |
                                                                                   v
                                                                            RiskManager (veto)
                                                                                   |
                                                                                   v
                                                                            ExecutionEngine
                                                                                   |
                                                                                   v
                                                                     REST /exchange → Hyperliquid
```

**Règle d'or** : aucune décision de trading ne dépend d'un état implicite dispersé. Chaque décision dépend d'un état **explicite, sérialisable, rejouable et auditable**. Tout événement important est journalisé avec timestamps locaux ET exchange.

### 4.3. Priorité des décisions (ordre strict)

1. Sécurité système (kill-switch, circuit breaker)
2. Cohérence de l'état (reconciliation exchange)
3. Gestion du risque (veto risk manager)
4. Gestion des positions ouvertes (lifecycle)
5. Nouvelles entrées (signal + exécution)
6. Optimisation du prix (amend, repricing)

---

## 5. Modules détaillés

### 5.1. Module `exchange`

Responsabilités :
- Encapsuler **toutes** les spécificités Hyperliquid
- Signatures EIP-712 (agent wallet / API wallet)
- Nonces, mapping asset id, tick/lot size par coin
- Ordres limit / ALO / IOC / GTC / trigger
- Lecture metadata, état compte, fills
- Reconnect WS et resync snapshot
- Mapping propre des erreurs de rejet

Points critiques :
- Distinguer **wallet signataire** (agent) et **adresse du compte** (subaccount)
- `orderUpdates` WS = **source primaire** des fills (sub-seconde). REST `userFillsByTime` = fallback uniquement
- Vérification du status `"err"` dans **chaque** réponse de trigger order
- Séparer les batches ALO des IOC/GTC (les batches ALO-only sont priorisés par le matching engine)

**Rate limiter** (leçon t-bot #15 — poids 3× trop permissifs) :

| Endpoint | Poids réel |
|----------|-----------|
| `candleSnapshot` | 20 + candles/60 |
| `openOrders`, `meta` | 20 |
| `allMids`, `clearinghouseState` | 2 |
| `/exchange` (ordre) | 1 |

Budget : 1200 weight/min. Un seul `RateLimiter` partagé entre tous les modules.

### 5.2. Module `market_data`

Flux WebSocket à souscrire :

```json
{"method": "subscribe", "subscription": {"type": "l2Book", "coin": "BTC"}}
{"method": "subscribe", "subscription": {"type": "trades", "coin": "BTC"}}
{"method": "subscribe", "subscription": {"type": "allMids"}}
{"method": "subscribe", "subscription": {"type": "orderUpdates", "user": "<wallet>"}}
```

Événements produits en interne :

| Événement | Usage |
|-----------|-------|
| `BookUpdate(coin, levels, ts)` | → Features, Regime, Strategy |
| `TradePrint(coin, price, size, side, ts)` | → Flow features, Tape |
| `MidUpdate(coin, mid)` | → PriceCache |
| `UserFill(oid, avg_px, size, ts)` | → OrderManager (chemin primaire) |
| `OrderAck(oid)` | → OrderManager |
| `OrderReject(oid, reason)` | → OrderManager |
| `ReconnectEvent` | → Suspension trading jusqu'à snapshot chargé |
| `SnapshotLoaded(coin)` | → Déblocage trading |

**Gestion de la saturation de queue** :
- `tokio::sync::mpsc` avec capacité 50_000 (BTC/SOL peuvent faire 50-100 updates/sec en session active)
- Si la queue sature sur un coin : `book_stale[coin] = true` → stratégie ne génère plus de signal pour ce coin
- **Ne pas dropper les updates book** : un book stale est plus dangereux qu'une pause momentanée
- Dropper les trades tape est acceptable (moins critique que le book)
- Logger et mesurer le lag (`ts_received - ts_exchange`)

**Reconnect** :
- Heartbeat ping toutes les 30s. Si aucun message depuis 60s → reconnect forcé
- Backoff exponentiel : 1s, 2s, 4s, 8s, max 30s
- Après reconnect : request snapshot REST full book **avant** de réappliquer les deltas
- Suspendre tout trading pendant la réhydratation

### 5.3. Module `state_store`

Contenu en mémoire :

```rust
struct StateStore {
    // Marché
    books: DashMap<String, OrderBook>,         // Best bid/ask, depth N niveaux
    trade_tape: DashMap<String, VecDeque<Trade>>, // Ring buffer 1000 trades/coin
    mids: DashMap<String, f64>,
    book_stale: DashMap<String, bool>,         // True si queue saturée ou reconnect en cours

    // Features calculées
    book_features: DashMap<String, BookFeatures>,
    flow_features: DashMap<String, FlowFeatures>,

    // Portefeuille
    positions: DashMap<String, OpenPosition>,
    pending_orders: DashMap<String, PendingOrder>,
    realized_pnl: AtomicF64,
    peak_equity: AtomicF64,
    daily_start_balance: AtomicF64,
}
```

Invariants :
- Séparer état marché et état portefeuille (jamais mélangés)
- Toute transition d'état est enregistrée (timestamp + raison)
- Protection contre les états partiellement mis à jour (transactions atomiques sur le portefeuille)

### 5.4. Module `features`

Voir section 6 pour le détail des features.

### 5.5. Module `regime`

Voir section 7 pour le détail.

### 5.6. Module `strategy`

La stratégie ne parle **jamais directement** à l'exchange. Elle émet des **intentions** :

```rust
enum Intent {
    NoTrade,
    PlacePassiveEntry { coin, direction, price, size, max_wait_s },
    AmendPassiveEntry { oid, new_price },
    CancelEntry       { oid, reason },
    PlacePassiveExit  { coin, price, size },
    ForceExitIOC      { coin, size, reason },
    ReducePosition    { coin, size },
    Cooldown          { coin, duration_s },
}
```

Le module `execution` convertit les intentions en ordres Hyperliquid concrets.

### 5.7. Module `risk`

**Le risk a droit de veto absolu sur la stratégie.**

Valide chaque intention avant envoi à l'exchange. Voir section 11 pour les règles complètes.

### 5.8. Module `execution`

Responsabilités :
- Conversion intention → ordre Hyperliquid (prix aligné tick, taille alignée lot)
- Choix ALO / IOC / GTC selon le contexte
- Placement, amend, cancel
- Suivi des fills partiels
- Timeout d'ordre
- Déduplication des requêtes
- Rate limiting local

**Amend vs Cancel+Replace** : utiliser `amend_order` (modification de prix) de préférence systématique. Cancel+Replace = 2 appels API + perte de position dans la queue. Amend = 1 appel API + conservation. Utiliser Cancel+Replace uniquement si la taille change aussi.

### 5.9. Module `portfolio`

Vérité interne du portefeuille :

```rust
struct PortfolioState {
    positions: HashMap<String, OpenPosition>,  // coin → position
    realized_pnl: Decimal,
    funding_cumulated: Decimal,
    fees_cumulated: Decimal,
    margin_used: Decimal,
    peak_equity: Decimal,
    daily_start_balance: Decimal,
    daily_reset_ts: i64,
}
```

Réconcilié périodiquement avec l'exchange (voir section 12.5).

---

## 6. Features microstructure

Calculées à chaque update du book et passées au regime engine et à la stratégie.

### 6.1. Features instantanées (snapshot book)

```rust
struct BookFeatures {
    // Spread
    spread_bps: f64,              // (ask - bid) / mid × 10000
    spread_vs_avg: f64,           // spread_bps / spread_avg_rolling (>1 = spread élargi)

    // Imbalance (plusieurs granularités)
    imbalance_top1: f64,          // (bid_qty - ask_qty) / (bid_qty + ask_qty), [-1, +1]
    imbalance_top3: f64,
    imbalance_top5: f64,
    imbalance_weighted: f64,      // Pondéré par distance au mid (niveaux proches = plus de poids)

    // Depth
    bid_depth_10bps: f64,         // Volume cumulé bid à 10 bps du mid
    ask_depth_10bps: f64,
    depth_ratio: f64,             // bid_depth / ask_depth
    book_slope_bid: f64,          // Comment la liquidité diminue en s'éloignant du top
    book_slope_ask: f64,

    // Micro-price et VAMP
    micro_price: f64,             // P_ask × Q_bid/(Q_bid+Q_ask) + P_bid × Q_ask/(Q_bid+Q_ask)
    micro_price_vs_mid_bps: f64,  // (micro_price - mid) / mid × 10000
    vamp: f64,                    // Volume-Adjusted Mid Price sur N niveaux
    vamp_signal_bps: f64,         // (vamp - mid) normalisé, en bps
}
```

**Formules clés** :

```
micro_price = ask × (Q_bid / (Q_bid + Q_ask)) + bid × (Q_ask / (Q_bid + Q_ask))

VAMP = Σ(P_bid_i × Q_ask_i + P_ask_i × Q_bid_i) / Σ(Q_ask_i + Q_bid_i)  sur N niveaux
```

### 6.2. Features temporelles (fenêtres roulantes)

```rust
struct FlowFeatures {
    // OFI — fenêtres multiples
    ofi_1s: f64,                  // (buy_vol - sell_vol) / (buy_vol + sell_vol), 1s
    ofi_3s: f64,
    ofi_10s: f64,
    ofi_30s: f64,

    // Trade aggression
    trade_intensity: f64,         // Trades / seconde (fenêtre 10s)
    avg_trade_size: f64,
    large_trade_ratio: f64,       // % trades > 2× taille moyenne
    aggression_persistence: f64,  // Proportion de trades dans la même direction (10 derniers)

    // Volatilité réalisée
    realized_vol_3s: f64,
    realized_vol_10s: f64,
    realized_vol_30s: f64,
    vol_ratio: f64,               // realized_vol_3s / realized_vol_30s (>1 = accélération)

    // Toxicity (proxy adverse selection)
    // Proportion de trades pour lesquels le mid a bougé dans la même direction dans les 5s
    // ⚠ DÉLAI INHÉRENT : valeur disponible à T+5s, pas à T
    // Utiliser comme filtre binaire (poster/ne pas poster), pas comme signal directionnel
    fill_toxicity_5s: f64,        // 0-1, sur les 100 derniers trades
    // Proxy instantané (sans lookahead) :
    toxicity_proxy_instant: f64,  // Proportion de trades tape qui ont consommé le best level

    // Refill speed
    refill_speed: f64,            // Vitesse de reconstitution de la liquidité après consommation

    // Cancel/add ratio (spoofing proxy)
    cancel_add_ratio: f64,        // Trop élevé = contexte peu sain
}
```

### 6.3. Queue desirability score

Score interne estimant si cela vaut la peine de se poster passif à un prix donné :

```
queue_score =
    w1 × (1 - spread_normalized)       // spread serré = mieux
  + w2 × |imbalance_weighted|          // imbalance favorable = mieux
  + w3 × (1 - fill_toxicity_5s)        // pas toxique = mieux
  + w4 × depth_ratio_favorable         // depth de notre côté = mieux
  - w5 × vol_ratio                     // vol explosive = risqué
```

Si `queue_score < seuil` → `Intent::NoTrade`.

---

## 7. Regime engine

Composant dédié qui classe le contexte marché **avant** toute décision de trading. La stratégie ne décide pas si le marché est tradable — c'est la responsabilité exclusive du regime engine.

### 7.1. Régimes

```rust
enum Regime {
    QuietTight,      // Spread serré, book profond, vol basse — idéal pour entrées maker
    QuietThin,       // Spread serré mais book peu profond — risque d'impact, prudence
    ActiveHealthy,   // Spread acceptable, vol normale, flow propre — tradable avec filtres
    ActiveToxic,     // Flow informatif, adverse selection élevée — ne pas poster
    WideSpread,      // Spread > seuil — edge maker insuffisant
    NewslikeChaos,   // Vol explosive, updates très fréquentes, book instable — stop
    LowSignal,       // Marché trop calme, pas assez d'edge — pas de trade
    DoNotTrade,      // Kill-switch, circuit breaker, reconnect, book stale
}
```

### 7.2. Critères de classification

| Critère | QuietTight | ActiveHealthy | ActiveToxic | DoNotTrade |
|---------|-----------|--------------|-------------|------------|
| spread_bps | < 2 | 2-8 | any | > 15 |
| fill_toxicity | < 0.4 | < 0.6 | > 0.7 | any |
| vol_ratio | < 1.5 | < 2.0 | any | > 4.0 |
| bid_depth_10bps | > $10K | > $5K | any | < $1K |
| book_stale | false | false | any | true |
| reconnect_recent | false | false | any | true |
| funding_boundary | > 5min | > 2min | any | < 1min |
| cancel_add_ratio | < 0.5 | < 0.8 | > 0.9 | any |

**Règle** : si **une seule** condition critique (`DoNotTrade`) est vraie → `Regime::DoNotTrade`, aucun trade.

### 7.3. Intégration avec la stratégie

```rust
match regime {
    Regime::DoNotTrade | Regime::ActiveToxic | Regime::NewslikeChaos => Intent::NoTrade,
    Regime::WideSpread | Regime::LowSignal => Intent::NoTrade,
    Regime::QuietThin => strategy.evaluate_with_reduced_size(features),  // Taille réduite
    Regime::QuietTight | Regime::ActiveHealthy => strategy.evaluate(features),
}
```

---

## 8. Stratégie MFDP V1

**Une seule stratégie en V1.** Ne pas implémenter plusieurs stratégies simultanées avant d'avoir validé l'edge de la première.

### 8.1. Philosophie

Le bot détecte un biais directionnel court terme via les features microstructure, attend un micro-pullback local, entre passivement en ALO, puis sort selon les règles définies.

### 8.2. Score directionnel

```rust
let direction_score =
    w1 * normalized_ofi_10s
  + w2 * micro_price_vs_mid_normalized
  + w3 * vamp_signal_normalized
  + w4 * aggression_persistence_signed
  + w5 * depth_ratio_signed
  - w6 * fill_toxicity_5s;

// Les poids w1..w6 sont calibrés offline sur les données enregistrées (section 16)
// Valeurs initiales conservatrices à ajuster après observation
```

Décision :
- `direction_score > seuil_long` → biais LONG
- `direction_score < seuil_short` → biais SHORT (symétrique négatif)
- Sinon → `Intent::NoTrade`

### 8.3. Condition de pullback

Une fois le biais détecté, **ne pas entrer immédiatement** :
- LONG : attendre que le prix revienne vers le bid ou near-bid (retrace 30-50% du dernier micro-move haussier)
- SHORT : miroir
- Si le pullback ne se produit pas dans `max_wait_pullback_s` → annuler le setup

### 8.4. Placement de l'entrée

```rust
// LONG : poster au best bid (ou best_bid + 1 tick si spread > 2 ticks pour améliorer la queue)
// SHORT : poster au best ask (ou best_ask - 1 tick)
// TIF : ALO (rejeté si cross — ne jamais devenir taker à l'entrée involontairement)

// Vérification anti-cross : le prix ne doit jamais améliorer le spread côté opposé
```

### 8.5. Sorties

| Type | Condition | Mode |
|------|-----------|------|
| TP passif | Atteint le prix cible | ALO limit |
| Stop logique | Mid adverse > seuil | Cancel pending → IOC |
| Signal inverse | direction_score franchit le seuil opposé | IOC défensif |
| Regime change | → ActiveToxic ou DoNotTrade | IOC défensif |
| Time stop | Position ouverte > `max_hold_s` | IOC défensif |
| Funding boundary | < 1min avant funding défavorable | IOC défensif |
| MAE (Max Adverse Excursion) | Perte > `max_mae_bps` | IOC défensif |

**Règle maker→taker** : le bot n'est pas "maker-only dogmatique". En sortie défensive, le taker est utilisé sans hésitation. Ne **jamais** rester en position par attachement à l'exécution maker.

### 8.6. Arbitrage des signaux

Règles centralisées dans `OrderManager.process_intent()` :

| Cas | Règle |
|-----|-------|
| Même coin, même direction | Prendre, pas de doublon de position |
| Même coin, directions opposées | Skip — signaux contradictoires = incertitude |
| Même coin, position déjà ouverte | Ignorer tout nouveau signal sur ce coin |
| Biais directionnel global saturé | Si `long_count >= MAX_DIRECTIONAL_BIAS (3)` → skip les nouveaux LONGs |
| BTC dump -3% en 15min | Bloquer nouveaux LONGs sur tous les alts |

### 8.7. Stale quote

Un ordre resting posté il y a 30s peut être fortement adverse si le contexte a changé. À chaque update book, vérifier :

```rust
// Dans on_book_update, pour chaque pending entry :
if features.fill_toxicity_5s > settings.toxicity_threshold {
    return Intent::CancelEntry { oid, reason: "ToxicFlow" };
}
if regime == Regime::DoNotTrade || regime == Regime::ActiveToxic {
    return Intent::CancelEntry { oid, reason: "RegimeChanged" };
}
if !strategy.signal_still_valid(order.signal, features) {
    return Intent::CancelEntry { oid, reason: "StaleQuote" };
}
```

---

## 9. State machine de trading

État unique par coin — jamais déduit implicitement depuis plusieurs drapeaux.

```rust
enum TradeState {
    Flat,
    SetupDetected { signal: Signal, detected_at: i64 },
    WaitingPullback { signal: Signal, expires_at: i64 },
    EntryWorking { oid: String, order: PendingOrder },
    EntryPartial { oid: String, filled_qty: Decimal, total_qty: Decimal },
    InPosition { position: OpenPosition },
    ExitWorking { oid: String, position: OpenPosition },
    ExitPartial { oid: String, remaining_qty: Decimal },
    ForceExit { reason: String },
    ErrorRecovery { since: i64, last_error: String },
    SafeMode,   // Aucun trading, attente intervention manuelle
}
```

**Règle** : aucune transition d'état sans enregistrement (timestamp + raison + état précédent). La sequence `EntryPartial` est un état de première classe — gérer les fills partiels explicitement, pas comme un cas dégénéré.

---

## 10. Exécution

### 10.1. Placement d'un ordre maker

```rust
async fn place_maker_order(signal: &Signal, book: &OrderBook) -> Result<OrderResult> {
    let tick = coin_meta.tick_size;
    let price = match signal.direction {
        Long  => {
            if book.spread_bps() > 2.0 * tick_in_bps {
                book.best_bid + tick   // Améliorer pour être en tête de queue
            } else {
                book.best_bid
            }
        },
        Short => { /* miroir */ }
    };

    // Vérification : le prix ne doit JAMAIS croiser le book (sinon l'ALO est rejeté)
    assert!(price <= book.best_bid || signal.direction == Short);

    // Arrondi tick/lot OBLIGATOIRE avant envoi
    let price = round_price_to_tick(price, tick);
    let size  = round_size_to_lot(size, coin_meta.lot_size);
    validate_order(coin, price, size)?;  // Rejeter avant envoi si invalide

    rest_client.place_order(coin, is_buy, price, size, Tif::Alo).await
}
```

### 10.2. Suivi des fills

**Chemin primaire — WS `orderUpdates`** :
```rust
// Callback WS → fill immédiat, latence sub-seconde
async fn on_order_update(&self, msg: OrderUpdateMsg) {
    match msg.status {
        "filled" => {
            let fill_price = msg.avg_px.unwrap_or(msg.px);
            // JAMAIS utiliser le prix théorique du signal — toujours le vrai fill price
            self.on_fill(msg.oid, fill_price, msg.filled_qty).await;
        },
        "cancelled" => self.on_cancel(msg.oid).await,
        "rejected"  => self.on_reject(msg.oid, msg.error).await,
        _ => {}
    }
}
```

**Chemin fallback — polling REST toutes les 5s** (si WS coupé) :
```rust
// Uniquement pour détecter les fills/cancels manqués par le WS
// Pas le chemin principal — ne pas y compter en conditions normales
```

### 10.3. Amend vs Cancel+Replace

| Situation | Action | Raison |
|-----------|--------|--------|
| Prix non-compétitif (seul le prix change) | **Amend** | 1 appel API, conservation queue position |
| Taille ET prix changent | Cancel+Replace | Amend ne peut pas changer la taille |
| Break-even SL (prix SL change) | **Amend** | Conservation queue position |
| Trailing stop | **Amend** | Conservation queue position |
| Toxic flow détecté | Cancel | Vitesse > conservation queue |

### 10.4. Client Order ID

Convention obligatoire, traçable et rejouable :

```
{strategy}-{coin}-{session_date}-{seq:06}-{intent}

Exemple : mfdp-btc-20260401-000421-entry
          mfdp-sol-20260401-000422-tp
          mfdp-btc-20260401-000423-sl
```

### 10.5. Fonctions numériques obligatoires

```rust
// Toutes les opérations de prix/taille passent par ces fonctions
fn round_price_to_tick(price: Decimal, tick: Decimal) -> Decimal
fn round_size_to_lot(size: Decimal, lot: Decimal) -> Decimal
fn min_valid_order_size(coin: &str, price: Decimal) -> Decimal
fn is_order_valid(coin: &str, price: Decimal, size: Decimal) -> Result<()>

// Règle : jamais de f64 pour la logique critique d'ordre
// Utiliser rust_decimal::Decimal pour tous les prix et tailles
```

---

## 11. Risk management

### 11.1. Principe

Le risk manager a **droit de veto absolu** sur la stratégie. Il valide chaque intention avant envoi à l'exchange.

### 11.2. Règles par trade

| Règle | Valeur défaut | Note |
|-------|--------------|------|
| Score directionnel minimum | configurable | Seuil calibré offline |
| SL obligatoire | oui | Rejet si absent |
| Max loss per trade | 1.5% du portfolio | Détermine le levier effectif |
| Max slippage toléré | 0.5% | Rejet si SL trop loin |
| Spread > seuil | > 10 bps | Pas de trade |
| Depth bid/ask minimum | > $5K à 10 bps | Pas de trade |
| Toxicity > seuil | > 0.7 | Pas de posting |
| Vol_ratio > seuil | > 3.0 | Pas de trade |

### 11.3. Règles portfolio

| Règle | Valeur défaut |
|-------|--------------|
| Max open positions | 5 |
| Max directional bias | 3 dans la même direction |
| Max margin usage | 60% de l'equity |
| Max daily loss | 10% |
| Drawdown throttle start | 7% → max positions ÷ 2 |
| Drawdown throttle severe | 12% → 1 position max |
| Drawdown circuit breaker | 20% → kill-switch |
| Pas de doublon coin | 1 seule position par coin |
| Cooldown après fermeture | 60s (scalp court terme) |

### 11.4. Gestion de l'equity et du peakEquity

```rust
// Drawdown calculé sur EQUITY (pas balance disponible) — leçon t-bot bug #12
let drawdown = (peak_equity - equity) / peak_equity * 100.0;

// Guard anti-spike : si equity saute de > 5% en 1 cycle → artefact API, ignorer
// Le peakEquity ne doit JAMAIS être basé sur une valeur non-validée
// (leçon tbot-scalp 2026-03-31 : race condition spot/perps → peakEquity gonflé → faux drawdown 13%)
if equity > peak_equity {
    let jump_pct = (equity - peak_equity) / peak_equity * 100.0;
    if jump_pct <= 5.0 {
        peak_equity = equity;
    } else {
        warn!("[RISK] Equity spike ignored: {:.2} → {:.2} (+{:.1}%) > 5% — likely API artifact",
              peak_equity, equity, jump_pct);
    }
}

// Reset quotidien à minuit UTC
// Cross-validation equity : si spotHold=0 mais marginUsed > 0 → race condition API
//   → fallback conservateur (spotTotal au lieu de spotBalance + accountValue)
// Un seul appel API pour total ET hold — ne jamais faire 2 appels séparés
```

### 11.5. Levier

```rust
// Risk-based leverage : cible max_loss_per_trade_pct du portfolio
let target_leverage = (max_loss_per_trade_pct * 100.0) / (sl_distance_pct * position_size_pct);
let effective_max_lev = min(config.max_leverage, coin_meta.max_leverage);
let leverage = clamp(target_leverage, config.min_leverage, effective_max_lev);

// Ne jamais utiliser le levier max théorique comme cible par défaut
// Ajuster par actif : BTC 40x théorique → ~20x opérationnel avec sl_dist=0.4%
```

### 11.6. Funding

```rust
// Vérifier le temps restant avant le prochain funding (toutes les heures sur Hyperliquid)
if time_to_funding_s < 60 && expected_funding < 0.0 {
    // Pas de nouvelle entrée si funding défavorable dans moins d'1 minute
    return Intent::NoTrade;
}
if time_to_funding_s < 30 && position.is_open() && expected_funding < -0.002 {
    // Forcer la sortie si on approche du funding significativement défavorable
    return Intent::ForceExitIOC { reason: "FundingBoundary" };
}
```

### 11.7. Corrélation inter-coin

```rust
// Bloquer les nouveaux LONGs sur les alts si BTC dump
let btc_move_15m = price_cache.get_move("BTC", Duration::from_secs(900));
if btc_move_15m < -0.03 && signal.direction == Long && signal.coin != "BTC" {
    return vec!["BTC sell-off in progress — no new LONG on alts".to_string()];
}
```

### 11.8. Kill-switch

Déclenché automatiquement si :
- Drawdown > `max_drawdown_pct`
- Nombre d'erreurs exchange > seuil sur 5 minutes
- Désynchronisation état interne / exchange
- Reconnects > N/heure
- Taux de rejects anormal
- Book stream absent depuis > 60s
- Divergence PnL interne / PnL exchange > seuil

---

## 12. Position lifecycle

### 12.1. Cycle de vie

```
SIGNAL
  → SETUP_DETECTED
  → WAITING_PULLBACK
  → ENTRY_WORKING
  → ENTRY_PARTIAL (fill partiel)
  → IN_POSITION
  → EXIT_WORKING
  → EXIT_PARTIAL (fill partiel sortie)
  → FLAT

Branches :
  ENTRY_CANCELLED → FLAT (timeout, toxic flow, stale quote)
  FORCE_EXIT      → FLAT (regime change, MAE, funding, signal inverse)
  ERROR_RECOVERY  → SAFE_MODE (divergence état, erreurs répétées)
```

### 12.2. Triggers TP/SL sur l'exchange

Dès qu'un fill est confirmé → poser les trigger orders TP/SL sur l'exchange comme filet de sécurité.

```rust
async fn on_fill(&self, order: &PendingOrder, fill_price: Decimal) {
    // Recalculer TP/SL proportionnellement si fill_price diffère du prix théorique
    // (leçon t-bot bug #2 : prix TP/SL périmés)
    let ratio = fill_price / order.signal.entry_price;
    let adjusted_sl = order.signal.stop_loss * ratio;
    let adjusted_tp = order.signal.take_profit * ratio;

    // Vérifier que chaque trigger order est bien posé (status != "err")
    // (leçon t-bot bug #4 : réponses trigger orders ignorées)
    let tp_result = place_trigger_order(TP, adjusted_tp).await?;
    let sl_result = place_trigger_order(SL, adjusted_sl).await?;

    if tp_result.is_err() || sl_result.is_err() {
        log::error!("Trigger order placement failed — entering SAFE_MODE");
        transition_to(TradeState::SafeMode);
    }

    // Stocker original_stop_loss séparément — ne jamais l'écraser avec le SL courant
    // (leçon t-bot bug #9)
}
```

### 12.3. Break-even

```rust
// Quand le prix atteint X% du TP → déplacer SL au prix d'entrée
// Utiliser AMEND (pas cancel+replace) si seul le prix SL change
// Stocker original_stop_loss séparément (bug #9)
// Au restart : détecter le BE par comparaison SL/entry (< 0.2% = BE appliqué) (bug #9)
```

### 12.4. Trailing stop

```rust
// Après break-even, SL monte par paliers :
// TP progress >= 65% → SL = entry + 25% du profit
// TP progress >= 80% → SL = entry + 50% du profit
// Chaque palier = AMEND du trigger order (pas cancel+replace)
```

### 12.5. Réconciliation avec l'exchange

```rust
async fn sync_with_exchange(&self) {
    let exchange_positions = match self.rest.get_open_positions().await {
        Ok(p) => p,
        Err(e) => {
            // JAMAIS interpréter une erreur API comme "0 positions"
            // (leçon t-bot bug #13 : faux SL_HIT sur erreur 429)
            log::error!("Sync failed: {} — skipping", e);
            return;
        }
    };

    // Safety guard : si exchange retourne 0 positions mais on en tracke > 0
    if exchange_positions.is_empty() && !self.positions.is_empty() {
        let recent_fills = self.rest.get_recent_close_fills(Duration::from_secs(7200)).await?;
        let closed_coins: HashSet<_> = recent_fills.iter().map(|f| &f.coin).collect();
        let tracked_coins: HashSet<_> = self.positions.keys().collect();

        if closed_coins.is_disjoint(&tracked_coins) {
            log::warn!("Exchange 0 positions but tracking {} — likely API error, skipping",
                      self.positions.len());
            return;
        }
        // Fermer uniquement les coins avec fills confirmés, garder les autres
    }

    // Détecter les positions orphelines (exchange mais pas trackées) → recover
    let orphans: Vec<_> = exchange_positions.iter()
        .filter(|p| !self.positions.contains_key(&p.coin))
        .collect();
    if !orphans.is_empty() {
        log::warn!("Orphan positions: {:?} — recovering", orphans);
        self.recover_positions().await;
    }
}
```

### 12.6. Nettoyage triggers orphelins (leçon tbot-scalp 2026-03-25)

Au startup : lister tous les trigger orders ouverts sur l'exchange, annuler ceux qui ne correspondent à aucune position trackée. Chaque coin ne doit avoir au plus que 2 triggers actifs (1 TP + 1 SL).

---

## 13. Backtest et replay

### 13.1. Niveaux de backtest

**Niveau 1 — Replay tick-by-tick (primaire)** :
Une fois le recorder (section data) actif depuis 2+ semaines, replay événementiel complet sur les données Parquet enregistrées. C'est le seul backtest représentatif pour ce type de bot.

**Niveau 2 — Backtest OHLCV (fallback initial)** :
Pendant la phase de développement initiale (avant données L2), backtest simplifié sur chandeliers. À utiliser uniquement pour tester la plomberie (risk management, lifecycle), pas pour valider l'edge.

```
Pénalités obligatoires dans les deux cas :
- Frais maker (0.015%) et taker (0.045%) appliqués
- Slippage entrée : 0.03%, sortie : 0.04%
- Fill rate ALO : calibré offline (défaut conservateur : 50%)
- Funding cumulé
```

### 13.2. Modèle de fill probabiliste

```rust
fn should_fill(order: &PendingOrder, book: &SimBook, elapsed_s: f64) -> bool {
    // 1. Le prix doit avoir été atteint
    if order.is_buy && book.best_ask > order.price { return false; }

    // 2. Queue position simplifiée (on est au milieu de la queue)
    let vol_traded = sim_book.volume_traded_at(order.price, since: order.placed_at);
    let vol_queued = sim_book.depth_at(order.price, at: order.placed_at);
    let fill_prob = (vol_traded / (vol_queued * 0.5)).min(1.0);

    // 3. Winner's curse : si on se fait filler, c'est souvent adverse
    let adverse_adjust = 1.0 - flow_features.fill_toxicity_5s * 0.3;

    rand::random::<f64>() < fill_prob * adverse_adjust
}
```

### 13.3. Métriques de validation

| Métrique | Description |
|----------|-------------|
| `expectancy_net` | P&L net moyen par trade après frais |
| `hit_rate` | % de trades positifs |
| `avg_winner / avg_loser` | Rapport gains/pertes |
| `max_adverse_excursion` | Pire mouvement adverse pendant la position |
| `maker_fill_rate` | % des ordres ALO effectivement fillés |
| `adverse_selection_rate` | % fills suivis d'un mouvement défavorable > 2 bps en 5s |
| `time_in_position` | Durée moyenne des positions |
| `pnl_by_regime` | P&L net par régime de marché |
| `pnl_by_hour` | P&L net par heure de la journée |
| `cancel_rate` | % ordres annulés vs fillés |
| `fee_drag` | % du P&L brut mangé par les frais |

**Critère de go/no-go** :
- `expectancy_net > $0` sur toutes les périodes out-of-sample
- `maker_fill_rate > 40%` (sinon la stratégie pullback ne fonctionne pas)
- `adverse_selection_rate < 60%` (sinon les features ne donnent pas d'edge)
- `fee_drag < 50%` (sinon les frais mangent tout)

---

## 14. Stockage, observabilité et UI

### 14.1. Données à enregistrer

**Obligatoires dès la phase 1** (sans ces données, pas de backtest possible) :

```
data/
├── l2/
│   └── {coin}/{YYYY-MM-DD}.parquet   — Book snapshots + deltas
├── trades/
│   └── {coin}/{YYYY-MM-DD}.parquet   — Tape complète
├── features/
│   └── {coin}/{YYYY-MM-DD}.parquet   — Features calculées (BookFeatures + FlowFeatures)
├── signals/
│   └── {YYYY-MM-DD}.parquet          — Signaux générés (fillés ou non)
├── orders/
│   └── {YYYY-MM-DD}.parquet          — Ordres envoyés + acks + rejects
├── fills/
│   └── {YYYY-MM-DD}.parquet          — Fills réels (prix, taille, timestamp exchange)
└── pnl/
    └── {YYYY-MM-DD}.parquet          — Timeline P&L, equity, drawdown
```

**Estimations de volume** : BTC/SOL/ETH en session active = 50-100 updates/sec. Compter **5-20 GB/jour compressé** pour les coins liquides. Prévoir 200 GB minimum pour 30 jours.

### 14.2. Logs structurés

Format JSON avec champs obligatoires :
```json
{
    "ts_local": 1743500000123,
    "ts_exchange": 1743500000098,
    "level": "INFO",
    "module": "execution",
    "coin": "BTC",
    "event": "order_placed",
    "oid": "mfdp-btc-20260401-000421-entry",
    "price": "66750.0",
    "size": "0.001",
    "latency_ms": 142
}
```

### 14.3. Métriques Prometheus

| Métrique | Description |
|----------|-------------|
| `ws_reconnect_total` | Nombre de reconnexions WS |
| `order_reject_total` | Ordres rejetés par l'exchange |
| `passive_fill_rate` | % ALO fillés vs annulés |
| `maker_share` | % des trades en mode maker |
| `adverse_selection_rate` | Proxy toxicity live |
| `spread_capture_bps` | Bps capturés net de frais |
| `queue_lag_ms` | Lag de la message queue (détection saturation) |
| `kill_switch_total` | Nombre de déclenchements kill-switch |
| `equity` | Equity courante |
| `drawdown_pct` | Drawdown courant |

### 14.4. UI de monitoring

#### Philosophie

L'UI n'est **pas** une interface de contrôle (pas de boutons "place order", "close position"). C'est un **tableau de bord de diagnostic en temps réel**, conçu pour répondre à une question centrale :

> *Le bot se comporte-t-il comme prévu ? Est-ce que je vois ce que je devrais voir ?*

Stack délibérément minimaliste : HTML + JS vanilla + CSS custom, servi statiquement par Axum. Pas de React, pas de bundler, pas de Node. Même approche que tbot — aucune friction de build.

**Statut** : ✅ Implémenté. Fichiers dans `static/` (index.html, css/styles.css, js/app.js). Servi par `tower_http::services::ServeDir` via Axum.

#### Architecture

```
Axum (Rust)
  GET /               → sert static/index.html (fallback ServeDir)
  GET /static/*       → sert CSS, JS (tower-http ServeDir)
  GET /api/state      → snapshot JSON complet (DashboardSnapshot)
  GET /api/stream     → SSE : push toutes les 500ms (unfold stream)
  GET /metrics        → Prometheus (scraping machine)
  GET /health         → healthcheck
```

Le frontend s'abonne au SSE `/api/stream` pour les mises à jour temps réel. Pas de polling — une seule connexion persistante.

**Implémentation** : `src/observability/dashboard.rs` expose un `DashboardState` contenant un `Arc<RwLock<DashboardSnapshot>>`. Le main loop écrit le snapshot toutes les 500ms via un `tokio::time::interval` dans le `select!`. Le SSE stream lit le `RwLock` et pousse le JSON sérialisé.

#### Payload SSE (`/api/stream`)

```json
{
  "ts": 1743500000123,
  "regime": "ActiveHealthy",
  "equity": 193.42,
  "drawdown_pct": 1.2,
  "daily_pnl": 0.84,
  "positions": [
    {
      "coin": "BTC", "direction": "LONG", "state": "InPosition",
      "entry_price": 66750.0, "current_price": 66820.0,
      "pnl_pct": 0.42, "pnl_usd": 0.18, "elapsed_s": 47,
      "break_even_applied": false, "sl": 66450.0, "tp": 67200.0
    }
  ],
  "pending_orders": [
    {
      "coin": "SOL", "direction": "SHORT", "state": "EntryWorking",
      "price": 81.20, "placed_s_ago": 4, "max_wait_s": 30
    }
  ],
  "books": {
    "BTC": {
      "spread_bps": 1.2, "imbalance_top5": 0.34,
      "micro_price_vs_mid_bps": 0.8, "toxicity": 0.31,
      "regime": "ActiveHealthy"
    },
    "SOL": {
      "spread_bps": 3.1, "imbalance_top5": -0.52,
      "micro_price_vs_mid_bps": -1.4, "toxicity": 0.61,
      "regime": "ActiveToxic"
    }
  },
  "metrics": {
    "maker_fill_rate_1h": 0.58, "adverse_selection_rate_1h": 0.41,
    "spread_capture_bps_session": 2.1, "ws_reconnects_today": 0,
    "queue_lag_ms_p95": 18, "kill_switch_count": 0
  }
}
```

#### Layout de l'UI (single page, pas d'onglets)

L'UI est une **single page verticale** avec 4 zones distinctes, toujours visibles simultanément — pas d'onglets. Sur un bot scalp 1m, tout est urgent, rien ne doit être caché.

```
┌─────────────────────────────────────────────────────────┐
│  HEADER : Equity | Daily P&L | Drawdown | Régime global │
│           Indicateur live (vert/rouge/gris)              │
├──────────────────────────┬──────────────────────────────┤
│  CARNET EN TEMPS RÉEL    │  POSITIONS OUVERTES          │
│  (par coin actif)        │  + ORDRES PENDING            │
│                          │                              │
│  BTC  spread:1.2bps      │  BTC LONG  +0.42%  47s      │
│       imb: +34%  ▶ ALO   │  SL: 66450  TP: 67200       │
│       tox: 0.31          │  [break-even: non]           │
│       régime: Healthy    │                              │
│                          │  SOL SHORT (pending 4s/30s)  │
│  SOL  spread:3.1bps      │  @ 81.20                     │
│       imb: -52%  ✗ TOXIC │                              │
│       tox: 0.61          │                              │
│       régime: Toxic      │                              │
├──────────────────────────┴──────────────────────────────┤
│  MÉTRIQUES SESSION                                       │
│  Fill rate: 58% | Adv. sel.: 41% | Spread cap: 2.1bps  │
│  Queue lag p95: 18ms | WS reconnects: 0 | Kill-sw: 0   │
├─────────────────────────────────────────────────────────┤
│  FEED D'ÉVÉNEMENTS (rolling, 30 dernières lignes)        │
│  [18:42:03] BTC LONG filled @ 66750 (lev 20x, $15.2)   │
│  [18:41:58] SOL pending SHORT @ 81.20 (ALO placed)      │
│  [18:41:45] SOL regime → ActiveToxic (tox=0.61)         │
│  [18:41:30] BTC break-even déclenché (SL → 66750)       │
└─────────────────────────────────────────────────────────┘
```

#### Zone "Carnet en temps réel"

Spécifique à ce bot — absent de tbot. Pour chaque coin actif :

| Indicateur | Affichage | Seuil coloration |
|------------|-----------|-----------------|
| Spread bps | valeur + tendance (↑↓) | vert < 3, orange 3-8, rouge > 8 |
| Imbalance top5 | barre horizontale [-1, +1] | vert si aligne position, rouge si contraire |
| Micro-price vs mid | bps, avec flèche directionnelle | vert si aligne, gris si neutre |
| Toxicity | 0.0 → 1.0, jauge colorée | vert < 0.4, orange 0.4-0.7, rouge > 0.7 |
| Régime | badge coloré | voir couleurs régime ci-dessous |
| Eligible ALO ? | ✓ / ✗ | vert/rouge selon régime + toxicity |

**Couleurs régime** :
- `QuietTight` → fond vert foncé
- `ActiveHealthy` → fond bleu
- `QuietThin` → fond jaune
- `ActiveToxic` → fond orange
- `WideSpread` / `LowSignal` → fond gris
- `NewslikeChaos` / `DoNotTrade` → fond rouge clignotant

#### Zone "Feed d'événements"

Rolling log des 30 derniers événements significatifs (pas tous les book updates — uniquement les événements de trading) :
- Fills (entrée/sortie), avec prix réel et latence
- Changements de régime
- Ordres pending placés / expirés / annulés
- Break-even, trailing stop déclenché
- Kill-switch, circuit breaker
- Reconnect WS

Chaque ligne est colorée par type : fill=bleu, régime=violet, risque=rouge, ordre=gris.

#### Ce que l'UI ne fait PAS

- Pas de graphique de prix ou de chandeliers (pas de données OHLCV)
- Pas de boutons de contrôle (l'UI est read-only)
- Pas de configuration runtime (tout passe par le fichier TOML)
- Pas d'historique des trades (c'est le rôle des fichiers Parquet + DuckDB offline)
- Pas d'authentification (l'UI est accessible en local uniquement — ne pas exposer publiquement)

#### Checklist UI

- [x] Header toujours visible avec indicateur live (rouge si WS book silencieux > 5s)
- [x] Régime par coin mis à jour < 1s après le changement
- [x] Feed d'événements en temps réel via SSE (pas de polling)
- [x] Responsive minimum (lisible sur un écran de laptop, pas forcément mobile)
- [x] Pas de dépendance externe (pas de CDN — si le réseau est coupé, l'UI doit quand même charger)

### 14.5. Alertes temps réel

Déclencher une alerte (Discord/Telegram webhook) si :
- Kill-switch activé
- Drawdown > seuil d'alerte (ex: 10%)
- N reconnects WS en 1 heure
- Divergence portefeuille interne/exchange
- Ordre rejeté répété sur le même coin
- Absence de données WS depuis > 60s
- `passive_fill_rate` < 20% sur 1h (stratégie potentiellement non viable)

---

## 15. Sécurité opérationnelle

### 15.1. Wallets

```
wallet_master   → JAMAIS exposé au bot, stocké offline
subaccount_bot  → compte dédié à ce bot (pas d'autres fonds)
api_wallet      → agent wallet pour signer les ordres
```

- L'API wallet signe les ordres
- Pour interroger l'état du compte : utiliser l'adresse du subaccount
- Procédure de rotation documentée et testée

### 15.2. Secrets

- Jamais en dur dans le code ni dans le Dockerfile
- Injection par variables d'environnement ou secret store
- Fichier `.env` uniquement pour le dev local (ajouté à `.gitignore`)

### 15.3. Isolation

- Subaccount dédié : une liquidation ne peut pas contaminer d'autres fonds
- Préférence pour une logique de risque de type isolated par position
- Ne pas mélanger des positions diverses en cross sans nécessité

### 15.4. Audit trail

Tout événement important reconstituable depuis les logs :
- Ordre envoyé (payload complet + signature)
- Réponse exchange
- Fill (prix réel, pas théorique)
- Décision de sortie (raison + état des features au moment de la décision)
- Kill-switch (raison + état portfolio)

---

## 16. Pipeline de recherche

**Critère de go/no-go absolu : ne pas implémenter de stratégie live avant d'avoir complété les étapes 1-3.**

### Étape 1 — Captation de données (2-4 semaines)

Faire tourner le bot en mode **observation pure** (pas d'ordres, juste recording) :
- Book L2, trades, features calculées → Parquet
- Au moins 2 semaines sur BTC, plus si possible
- Couvrir plusieurs régimes : trending, ranging, news event

### Étape 2 — Analyse exploratoire

Objectifs :
- Distribution des spreads par heure de la journée
- Régimes de volatilité et leur fréquence
- Qualité de la depth (moments de thin book)
- Comportement de la toxicity
- Fill rate ALO estimé sur des ordres tests (dry-run)

### Étape 3 — Étude de prédictibilité

Tester sur les données enregistrées :
- OFI_1s, OFI_3s, OFI_10s → rendement futur 1s, 3s, 10s, 30s
- Micro-price edge → direction future
- VAMP signal → direction future
- Combinaisons → robustesse selon régime

**Critère de go** :
- Au moins une feature montre un pouvoir prédictif statistiquement significatif (p < 0.05, out-of-sample)
- L'edge survit au coût de frais simulé (expectancy nette > 0)
- L'adverse selection rate < 60% sur les coins cibles

### Étape 4 — Calibration des seuils

À partir des données et des études de prédictibilité :
- Calibrer les poids `w1..w6` du direction_score
- Calibrer les seuils `seuil_long`, `seuil_short`
- Calibrer les seuils du regime engine
- Valider out-of-sample (jamais ajuster les seuils sur le jeu de validation)

### Étape 5 — Replay paper trading

Rejouer session par session avec le moteur complet :
- Comparer ce que le bot aurait décidé vs ce qui s'est passé réellement
- Mesurer les métriques de validation (section 13.3)
- Identifier les faux positifs par régime

### Étape 6 — Live pilot ultra-réduit

- 1 seul actif (BTC ou SOL selon les résultats de l'étape 3)
- Taille minimale ($11 notional minimum Hyperliquid)
- Levier très bas (5x max)
- Horaires limités (sessions actives seulement : 14h-20h UTC)
- Surveillance renforcée pendant 1-2 semaines
- Critère de sortie : comportement stable, pas de divergence état/exchange, drawdown < 5%

---

## 17. Ordre de développement

```
✅ Phase 0 — Design (1 jour)
  Livrables : dictionnaire d'événements, définitions features, state machine,
              politique de risque, mapping erreurs exchange

✅ Phase 1 — Connectivité Hyperliquid (4-6 jours)
  Objectifs : WS stable + reconnect, metadata coins, état compte,
              ordre de test sur testnet, signatures EIP-712, rate limiter
  Critère de sortie : 0 désynchronisation sur 24h, ordres de test fiables

✅ Phase 2 — State store + recorder Parquet (3-4 jours)
  Objectifs : book local cohérent, écriture Parquet (arrow v53), snapshots réguliers,
              cancel/add ratio (rolling 60s), funding boundary dans regime engine
  Critère de sortie : replay session sans trous majeurs

✅ Phase 3 — Features + regime + dashboards (3-5 jours)
  Objectifs : calcul streaming features, regime engine, dashboard de monitoring
  Critère de sortie : features stables, interprétables, distributions raisonnables

  Phase 4 — OBSERVATION PURE (2-4 semaines)
  Faire tourner uniquement le recording et les features, AUCUN ordre
  Constituer le dataset pour la recherche (section 16)
  ⚠ À faire avant calibration des seuils stratégie

✅ Phase 5 — Risk management + exécution maker (4-6 jours)
  Objectifs : toutes les règles risk, placement ALO, amend, cancel,
              fill detection WS + fallback REST, sizing via equity réelle
  Critère de sortie : test complet sur testnet (place, amend, cancel, fill)

✅ Phase 6 — Position lifecycle (4-6 jours)
  Objectifs : state machine complète, TP/SL triggers (open_position_with_triggers),
              break-even cancel+replace (update_sl_trigger), trailing, sync exchange,
              orphan detection, recovery au restart, asset_index par coin
  Critère de sortie : kill + restart → positions récupérées, triggers toujours posés

✅ Phase 7 — Stratégie MFDP V1 + backtest (3-5 jours)
  Objectifs : stratégie avec SL/TP dans l'Intent, sizing via RiskManager,
              BacktestRunner (replay JSONL → pipeline complet → BacktestSummary)
  Critère de sortie : expectancy nette > 0, maker_fill_rate > 40%

  ✅ Phase 7.1 — Bug fixes post dry-run v1 (1-2 jours)
  Problèmes identifiés lors du dry-run du 2026-04-01 (WR=12%, 7/8 SL_HIT à -0.30%)

  a) Fix spread_bps négatif
     - 14/16 signaux placés ont un spread_bps < 0 → le book est inversé (ask < bid)
     - Cause probable : book stale ou delta appliqué sans snapshot → best_bid et best_ask incohérents
     - Fix : guard dans spread_bps() → retourner None si ask <= bid
     - Fix : guard dans compute_features() → skip si book pas snapshot_loaded ou spread négatif
     - Fix : marquer book_stale=true si spread négatif détecté (symptôme de données corrompues)

  b) Fix normalisation OFI saturée
     - OFI_10s vaut 1.0 ou -1.0 dans la majorité des signaux → pas de nuance
     - Cause : la normalisation (buy-sell)/(buy+sell) sature dès qu'un côté domine légèrement
     - Options : (1) utiliser le volume brut non-normalisé + z-score rolling, (2) clamp plus tard
       avec un seuil plus large, (3) utiliser OFI cumulé (ΔQ_bid - ΔQ_ask) au lieu du ratio

  c) Fix vol_ratio = 0.0
     - Moitié des signaux ont vol_ratio=0.0 → realized_vol_30s n'a pas assez d'échantillons
     - Probablement les premiers seconds après startup ou coins peu actifs
     - Fix : ne pas émettre de signal si les features temporelles ne sont pas matures
       (exiger N échantillons minimum dans les fenêtres roulantes avant toute évaluation)

  d) Fix aggression toujours 0.5-1.0
     - Jamais < 0.5 → le calcul est biaisé ou la fenêtre est trop courte
     - Vérifier la formule (proportion de trades dans la même direction sur les 10 derniers)
     - Si la fenêtre est trop courte (< 10 trades), l'aggression est bruiteuse → exiger un minimum

  Critère de sortie : 0 signal avec spread_bps négatif, OFI distribué sur [-1,+1] avec variance,
                       vol_ratio > 0 pour tous les signaux émis

  ✅ Phase 7.2 — SL/TP dynamique basé sur la volatilité (2-3 jours)
  Problème : SL fixé à 0.30% (pullback_retrace_pct) pour TOUS les coins, quelle que soit
  la volatilité. 0.30% = 30 bps ≈ bruit normal pour la plupart des coins. Résultat : 7/8
  trades fermés par SL immédiatement (dans les secondes qui suivent l'entrée).

  a) SL adaptatif par realized_vol
     - SL_distance = max(sl_min_bps, N × realized_vol_30s)
     - N calibré pour que le SL soit hors du bruit normal (ex: N=2.5 → ~99% du bruit 30s)
     - sl_min_bps = plancher pour éviter un SL à 0 en marché mort (ex: 10 bps)
     - sl_max_bps = plafond pour ne pas risquer trop (ex: 80 bps)

  b) TP dynamique avec R:R configurable
     - TP_distance = SL_distance × target_rr (défaut : 2.0)
     - R:R et SL calibrés par coin tier (BTC plus serré, small caps plus large)
     - À terme : R:R optimal par régime (QuietTight → R:R 3:1, ActiveHealthy → R:R 1.5:1)

  c) MAE adaptatif
     - max_mae_bps (actuellement 15 bps fixe) devrait aussi être proportionnel à realized_vol
     - MAE = SL_distance × mae_pct (ex: 50% du SL → si SL=40bps, MAE=20bps)

  d) Supprimer pullback_retrace_pct comme paramètre de SL
     - Ce paramètre contrôle actuellement deux choses (le retrace attendu ET le SL) → séparer
     - Le retrace attendu (entrée) et la distance SL (risk) sont des concepts différents

  Critère de sortie : SL varie par coin selon volatilité réalisée, aucun signal avec SL < 20 bps,
                       backtest amélioré vs fixed SL

  ✅ Phase 7.3 — Direction score : conviction et confirmation (2-3 jours)
  Problème : les scores directionnels sont juste au-dessus du seuil (0.50-0.67), indiquant
  une conviction faible. La stratégie entre trop facilement.

  a) Augmenter le seuil direction_threshold
     - Passer de ±0.50 à ±0.55 ou ±0.60
     - À calibrer sur les données collectées : quel seuil sépare les trades gagnants des perdants ?

  b) Exiger la persistance du signal (confirmation)
     - Ne pas entrer sur un seul tick d'OFI favorable
     - Exiger que le direction_score reste > seuil pendant N mises à jour consécutives (ex: 3-5)
     - Implémentation : compteur par coin, reset si score passe sous le seuil
     - Effet : filtrer les spikes de bruit, ne garder que les tendances micro établies

  c) Score de qualité des features
     - Avant de calculer le direction_score, vérifier que les features sont fiables :
       - spread_bps > 0 (book sain)
       - vol_ratio > 0 (données temporelles matures)
       - trade_intensity > seuil (assez de trades pour que l'OFI soit significatif)
     - Si une feature est "stale" ou absente → réduire son poids dans le score (pas la forcer à 0)

  d) Feature decorrelation
     - OFI, micro_price, VAMP sont potentiellement corrélés (tous mesurent un biais directionnel)
     - Vérifier empiriquement la corrélation sur les données collectées
     - Si r > 0.8 entre deux features → n'en garder qu'une (ou combiner via PCA)

  Critère de sortie : dir_score moyen des trades placés > 0.60, réduction de 30%+ des faux signaux

  ✅ Phase 7.4 — Analyse offline des données collectées (3-5 jours, Python)
  ⚠ Phase critique — conditionne la calibration de 7.2 et 7.3.
  Utiliser les données accumulées (L2, trades, features, signaux) pour calibrer empiriquement.

  a) Distribution des features par régime
     - Histogrammes : spread_bps, OFI_10s, micro_price_vs_mid, toxicity par régime
     - Identifier les features discriminantes vs bruitées

  b) Pouvoir prédictif des features
     - Pour chaque feature : corrélation avec mid_move_Ns (N = 1s, 5s, 10s, 30s)
     - Tableau feature × horizon → Spearman rank correlation (+ IC 95%)
     - Quelles features prédisent une direction ? À quel horizon ?

  c) Analyse adverse selection
     - Sur les fills dry-run : combien move dans la bonne direction vs mauvaise à +5s, +10s, +30s ?
     - Si adverse_selection > 60% → le signal n'a pas d'edge, ajuster les features

  d) Optimisation SL/TP
     - Sur les trades fermés : quel SL/TP maximise l'expectancy nette ?
     - MAE/MFE analysis : quelle excursion adverse/favorable est typique par coin ?
     - Résultat → paramétrer sl_min_bps, sl_max_bps, sl_vol_multiplier, target_rr

  e) Performance par coin
     - Quels coins sont profitables ? Quels coins drainent le P&L ?
     - Réduire l'univers aux coins avec edge démontré

  f) Performance par heure UTC
     - Certaines heures ont-elles un meilleur edge ? (sessions active Asia/EU/US)
     - Potentiel de filtre horaire pour éviter les heures mortes

  Livrables : notebook Python avec résultats, paramètres calibrés pour phases 7.2/7.3

  ✅ Phase 7.5 — Entry timing : vrai pullback + flow confirmation (2-3 jours)
  Problème : le bot entre dès que le direction_score franchit le seuil. La section 8.3 du plan
  décrit un mécanisme de "wait for pullback" mais l'implémentation actuelle ne vérifie pas
  de vrai retrace — elle entre immédiatement au best bid/ask.

  a) Détection de micro-move
     - Tracker le high/low récent (fenêtre 10-30s) pour chaque coin
     - Un micro-move haussier = nouveau high 10s. Baissier = nouveau low 10s.
     - Le pullback = retrace de X% du micro-move (ex: 30-50%)
     - Ne placer l'ordre ALO qu'une fois le pullback confirmé

  b) Flow confirmation post-pullback
     - Après le pullback, attendre que l'OFI repasse en faveur de la direction initiale
     - Cela confirme que le pullback est une respiration, pas un renversement
     - Séquence complète : direction_score > seuil → micro-move → pullback → OFI reprend → entry

  c) Abandon de setup
     - Si le pullback ne vient pas dans max_wait_pullback_s (30s) → abandon
     - Si le pullback dépasse 100% du micro-move → c'est un renversement, pas un pullback → abandon
     - Si un nouveau signal opposé apparaît pendant l'attente → abandon

  Critère de sortie : les entrées ne se font plus au premier tick, mais après un retrace confirmé.
                       Le taux de SL immédiat (< 30s) diminue de > 50%

  ✅ Phase 7.6 — Backtest replay amélioré (2-3 jours)
  Le BacktestRunner actuel est simplifié (JSONL replay). Les données L2 collectées permettent
  un backtest beaucoup plus réaliste.

  a) Replay L2 tick-by-tick
     - Lire les fichiers data/l2/{coin}/{date}.jsonl dans l'ordre chronologique
     - Reconstruire le book localement → calculer les features → passer au strategy
     - Simuler les fills ALO avec le modèle probabiliste (section 13.2)

  b) Modèle d'adverse selection dans le backtest
     - Chaque fill ALO simulé a un taux d'adverse selection calibré sur les données réelles
     - Ne pas surestimer la profitabilité maker (winner's curse)

  c) Comparaison SL fixe vs dynamique
     - Rejouer les mêmes données avec SL fixe (actuel) vs SL volatility-based (phase 7.2)
     - Mesurer l'amélioration de l'expectancy nette

  Phase 8 — Live pilot (continu)
  Ultra-réduit, 1 actif, surveillance renforcée (voir section 16 étape 6)
  Pré-requis : phases 7.1-7.5 validées, backtest phase 7.6 montrant expectancy nette > 0

  Phase 9 — UI de monitoring + alertes (2-3 jours)
  Backend : Axum, endpoints /api/stream SSE, /api/state, /metrics Prometheus
  Frontend : HTML/JS vanilla (index.html + static/), 4 zones (header, carnet, positions, feed)
  Alertes : webhooks Discord/Telegram pour kill-switch, drawdown, reconnects
```

> **Note sur les timelines** : les phases 1, 5 et 6 sont les plus risquées. Elles concentrent la quasi-totalité des bugs observés sur t-bot et tbot-scalp. Les phases 0–3 et 5–7 sont implémentées ; la phase 4 (observation pure + calibration) reste à faire avant le live.
>
> **Post dry-run v1 (2026-04-01)** : les phases 7.1–7.6 ont été ajoutées après le premier dry-run. Le dry-run a révélé que l'observabilité fonctionne (journal, signaux, fills simulés), mais la stratégie elle-même a un WR de 12% avec 7/8 trades fermés par SL à -0.30%. Causes identifiées : (1) SL fixe trop serré pour le bruit normal, (2) spread_bps négatifs (book inversé), (3) features saturées (OFI, vol_ratio), (4) pas de vrai pullback avant entrée. L'ordre des phases respecte les dépendances : bugfixes → SL dynamique → calibration direction → analyse offline → entry timing → backtest amélioré.

---

## 18. Leçons des bugs t-bot / tbot-scalp

Catalogue complet intégré directement dans le design pour ne pas reproduire ces erreurs.

### 18.1. Bugs d'exécution

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #1 | `checkNaturalClose` ne fermait pas sur l'exchange | Positions fantômes | `close_position()` appelle toujours l'exchange en live |
| t-bot #2 | Prix TP/SL non recalculés après fill | TP/SL décalés | Recalcul proportionnel au fill price systématique (section 12.2) |
| t-bot #7 | Break-even SL pas mis à jour sur l'exchange | Ancien SL actif après restart | AMEND du trigger order. Stocker `original_stop_loss` séparément |
| t-bot #9 | Break-even perdu au restart | SL reset à l'original | Détection automatique par comparaison SL/entry (< 0.2% = BE appliqué) |
| scalp 03-31 | Exit price théorique au lieu du vrai fill | PnL incorrect | Parser `avgPx` du WS `orderUpdates` — jamais le prix théorique |

### 18.2. Bugs de communication exchange

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #4 | Réponses trigger orders ignorées | Erreurs silencieuses | Check du status `"err"` dans chaque réponse trigger order |
| t-bot #13 | `getOpenPositions()` retournait `[]` sur 429 | Faux SL_HIT → positions orphelines | Propager l'exception. Safety guard 0 positions exchange + N trackées (section 12.5) |
| t-bot #14 | Thread scheduler mort (RestTemplate sans timeout) | Bot brain-dead | Timeout 30s sur toutes les requêtes HTTP. `tokio` résout le single-thread nativement |
| t-bot #15 | Rate limiter 3× trop permissif | 429 systématiques | Poids réels : candleSnapshot=20+, meta=20, allMids=2 (section 5.1) |
| scalp 03-25 | Trigger orders orphelins après crash | Doublons TP/SL | Nettoyage au startup (section 12.6) |

### 18.3. Bugs de calcul de risque

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #12 | Drawdown calculé sur `availableBalance` au lieu d'`equity` | Faux throttle | Toujours utiliser equity pour le drawdown (section 11.4) |
| scalp 03-31 | Race condition equity (2 appels API séparés) | peakEquity gonflé → faux drawdown 13% → maxPos=1 | Un seul appel `spotClearinghouseState`, cross-validation si hold=0 mais margin > 0 (section 11.4) |
| scalp 03-31 | Guard anti-spike peakEquity absent | peakEquity corrompu | Guard si equity jump > 5% en 1 cycle → ignorer (section 11.4) |

### 18.4. Bugs de données

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #8 | Leverage identique pour tous les coins | Levier trop élevé small caps | `effectiveMaxLev = min(config, coin_meta.max_leverage)` |
| t-bot #11 | xyz assets pas chargés dans `refreshMeta()` | xyz rejetés en live | Hors scope V1 (univers restreint BTC/SOL/ETH) |
| t-bot #3 | Hardcodé Kraken dans sync | Crash | Un seul exchange, pas d'abstraction multi-exchange |

### 18.5. Erreurs d'architecture

| Problème | Impact | Design L2 |
|----------|--------|-----------|
| `RestTemplate` sans timeout (Java) | Appels bloquants infinis | `reqwest` avec `timeout(Duration::from_secs(30))` sur toutes les requêtes |
| Spring scheduler pool size = 1 | 1 thread mort = tout meurt | `tokio` : tâches indépendantes, un panic dans une tâche ne tue pas les autres |
| Try-catch manquant sur les tâches | Crash silencieux | Wrapper générique sur chaque tâche longue : log + continue |
| Données stale non détectées | Signaux sur données périmées | Book temps réel + heartbeat WS + `book_stale` flag |
| peakEquity sur valeur non-validée | Faux drawdown chronique | Guard anti-spike (section 11.4) |

---

## 19. Checklist pré-live

### Avant d'activer le live trading

- [ ] **Observation pure stable 2 semaines** : recording L2 sans gaps, features stables
- [ ] **Edge validé** : études de prédictibilité OFI/micro-price positives out-of-sample
- [ ] **Maker fill rate mesuré** > 40% sur ordres tests dry-run (sinon stratégie non viable)
- [ ] **Adverse selection rate mesuré** < 60% sur coins cibles (sinon edge insuffisant)
- [ ] **Dry-run stable 48h** sans crash, reconnect WS fonctionnel, 0 erreur 429
- [ ] **Rate limiter validé** : 0 erreurs 429 sur 24h
- [ ] **Equity calculation validée** : comparer `getEquity()` avec dashboard Hyperliquid (5 checks manuels)
- [ ] **Recovery testé** : kill process → restart → positions récupérées, SL/TP toujours posés, pas de doublons
- [ ] **Circuit breaker testé** : simuler drawdown 20% → tous les ordres annulés, aucun nouveau trade
- [ ] **Trigger order placement validé** : poser TP/SL, vérifier sur HL, cancel → vérifier disparition
- [ ] **Amend validé** : amender un ordre resting → vérifier que le nouveau prix est actif
- [ ] **Kill-switch validé** : déclencher manuellement → plus aucun ordre pendant 10 minutes
- [ ] **Alertes opérationnelles** : webhook Discord/Telegram reçu pour kill-switch, drawdown seuil, reconnect
- [ ] **UI chargée et à jour** : SSE connecté, régimes affichés par coin, feed d'événements actif
- [ ] **Subaccount dédié configuré** : fonds séparés du wallet principal
- [ ] **Arrondi tick/lot testé** sur chaque coin autorisé (ordres invalides = rejet silencieux HL)

### Monitoring continu post-live (7 premiers jours)

- [ ] Spread moyen capturé > frais round-trip (sinon perte structurelle)
- [ ] Win rate > 50% (nécessaire pour un bot maker avec petit edge)
- [ ] Max drawdown < 5% sur les 7 premiers jours
- [ ] Maker fill rate > 40% en conditions réelles
- [ ] Pas de positions orphelines (sync exchange OK)
- [ ] Data recorder fonctionne (fichiers Parquet générés quotidiennement)
- [ ] Pas de kill-switch intempestif (vérifier les seuils de drawdown/throttle)

---

*Ce plan synthétise les meilleures idées de plan_v1.md et plan_v2.md, avec les corrections issues de l'expérience live de t-bot et tbot-scalp.*
