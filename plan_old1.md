# tbot-scalp-l2 — Plan de développement

> Bot de **scalping directionnel L2** pour Hyperliquid perps, en Python asyncio.
> Objectif : utiliser les signaux microstructurels (imbalance, depth, micro-price, toxicity) pour prendre des paris directionnels **avec entrée maker (ALO)** afin de minimiser les frais.
>
> **Note terminologique** : ce bot n'est PAS un market maker au sens strict (pas de bid+ask simultanés, pas de gestion d'inventaire bilatérale). C'est du scalping directionnel piloté par les données L2, avec exécution maker sur l'entrée. Ce choix est délibéré : le vrai market making require une gestion d'inventaire bilatérale complexe et une latence sub-100ms difficile à atteindre sans infrastructure dédiée. Le scalping directionnel L2 garde l'avantage des frais maker tout en restant implémentable à latence normale (~200ms).

---

## Table des matières

1. [Pourquoi un nouveau projet](#1-pourquoi-un-nouveau-projet)
2. [Stack technologique](#2-stack-technologique)
3. [Architecture globale](#3-architecture-globale)
4. [Phase 0 — Squelette et infra](#4-phase-0--squelette-et-infra)
5. [Phase 1 — Market data L2 (WebSocket)](#5-phase-1--market-data-l2-websocket)
6. [Phase 2 — Features microstructure](#6-phase-2--features-microstructure)
7. [Phase 3 — Stratégies](#7-phase-3--stratégies)
8. [Phase 4 — Exécution (ordres maker)](#8-phase-4--exécution-ordres-maker)
9. [Phase 5 — Risk management](#9-phase-5--risk-management)
10. [Phase 6 — Position lifecycle](#10-phase-6--position-lifecycle)
11. [Phase 7 — Backtest](#11-phase-7--backtest)
12. [Phase 8 — Dashboard & monitoring](#12-phase-8--dashboard--monitoring)
13. [Phase 9 — Déploiement & opérations](#13-phase-9--déploiement--opérations)
14. [Leçons des bugs t-bot / tbot-scalp](#14-leçons-des-bugs-t-bot--tbot-scalp)
15. [Checklist pré-live](#15-checklist-pré-live)

---

## 1. Pourquoi un nouveau projet

tbot-scalp est un bot chandelier (OHLCV → indicateurs lissés → signal → exécution). Sur 1m/3m, cette approche est structurellement handicapée :

- **Signaux retardataires** : EMA, RSI, Bollinger sont calculés sur des bougies fermées → le signal arrive après que le move a commencé
- **Pas de vision du carnet** : pas de spread, pas d'imbalance, pas de depth → impossible de distinguer un breakout sain d'un fakeout dans un book fin
- **Exécution aveugle** : le bot sait quand entrer mais pas à quel prix le book peut absorber → slippage imprévisible
- **Backtest trompeur** : les backtests OHLCV surestiment la rentabilité du scalp (pas de frais modélisés avant le fix du 2026-03-30, et surtout pas de slippage dynamique lié à la depth)

Un bot L2 inverse la logique : **le signal vient du carnet, l'exécution est le carnet**.

---

## 2. Stack technologique

### Langage : Python 3.12+

| Raison | Détail |
|--------|--------|
| Latence suffisante | Hyperliquid a ~200ms de latence e2e médiane. Python asyncio tourne en <10ms de traitement |
| Écosystème crypto | `hyperliquid-python-sdk`, `websockets`, `aiohttp` prêts à l'emploi |
| Prototypage rapide | Itérer sur les features microstructure (imbalance, VAMP, micro-price) sans cérémonie Java |
| Calcul vectoriel | `numpy` pour les calculs sur arrays de depth/trades, `pandas` pour l'analyse offline |

### Dépendances cibles

```
# Core
python = ">=3.12"
asyncio                    # event loop (stdlib)
websockets                 # WS client persistant vers Hyperliquid
aiohttp                    # HTTP async pour REST (/info, /exchange)
orjson                     # JSON rapide (5-10x stdlib json)
msgpack                    # Signing Hyperliquid (msgpack de l'action)

# Crypto / Signing
eth-account                # EIP-712 signing (keccak256, sign_message)
eth-abi                    # ABI encoding pour le struct hash

# Calcul
numpy                      # Arrays de depth, trades, features
 
# Monitoring
aiofiles                   # Écriture async des logs/journal
structlog                  # Logging structuré JSON

# Dev / Test
pytest                     # Tests
pytest-asyncio             # Tests async
uvloop                     # Event loop optimisé (Linux, optionnel)
```

### Ce qu'on n'utilise PAS

| Outil | Raison du rejet |
|-------|-----------------|
| Flask / FastAPI (au départ) | Le dashboard vient en phase 8, pas besoin d'un framework web au début |
| ccxt | Trop générique, cache la mécanique Hyperliquid (trigger orders, ALO, xyz dex) |
| pandas en hot path | Trop lent pour du temps réel — numpy brut dans le hot path, pandas en analyse offline |
| SQLAlchemy / DB | Même approche que t-bot : tout in-memory + JSONL pour la persistance |
| Threading | asyncio single-thread est suffisant et plus simple (pas de locks) |

---

## 3. Architecture globale

```
tbot-scalp-l2/
├── config/
│   ├── settings.py          — Pydantic Settings (équivalent TradingConfig)
│   └── coins.py             — Coin metadata (asset index, sz decimals, max leverage, xyz flag)
├── exchange/
│   ├── ws_client.py         — WebSocket persistant (L2 book, trades, allMids)
│   ├── rest_client.py       — REST async (/info, /exchange) avec rate limiter intégré
│   ├── signer.py            — EIP-712 Phantom Agent signing (port du Java HyperliquidSigner)
│   └── rate_limiter.py      — Token bucket async (1200 weight/min, weights réels par endpoint)
├── book/
│   ├── order_book.py        — Book local par coin : bids[], asks[], spread, mid, micro_price
│   ├── book_manager.py      — Reçoit les deltas WS, reconstruit le book, calcule les features
│   └── features.py          — Calcul des features micro (imbalance, VAMP, depth, spread, toxicity)
├── strategy/
│   ├── base.py              — Interface Strategy (on_book_update, on_trade)
│   ├── micro_mean_revert.py — Mean reversion pilotée par micro-price vs mid
│   ├── imbalance_fade.py    — Fade l'imbalance extrême (absorption)
│   ├── breakout_flow.py     — Breakout confirmé par order flow + depth
│   └── pullback_maker.py    — Pullback directionnel avec entrée maker
├── execution/
│   ├── order_manager.py     — Place/annule les ordres, track les pending fills
│   ├── position_manager.py  — Suivi des positions, break-even, trailing, lifecycle
│   └── risk_manager.py      — Validation pré-trade + portfolio risk
├── backtest/
│   ├── replay_engine.py     — Replay de fichiers L2 tick-by-tick
│   ├── sim_book.py          — Book simulé pour backtest (queue position modélisée)
│   └── sim_execution.py     — Exécution simulée avec fill probability, queue, adverse selection
├── data/
│   ├── recorder.py          — Enregistre les données L2/trades brutes en fichiers (backtest futur)
│   └── loader.py            — Charge les données enregistrées pour replay
├── journal/
│   ├── trade_journal.py     — JSONL des ordres (exécutés + rejetés)
│   └── trade_history.py     — JSONL des trades fermés (avec P&L)
├── monitoring/
│   ├── dashboard.py         — FastAPI + SSE (phase 8)
│   └── metrics.py           — Métriques internes (latence, fill rate, spread moyen)
├── main.py                  — Point d'entrée, bootstrap asyncio
├── requirements.txt
├── Dockerfile
└── tests/
```

### Flux de données (event-driven)

```
Hyperliquid WS ──┬── l2Book updates ──→ BookManager ──→ Features ──→ Strategies ──→ OrderManager
                  ├── trades          ──→ BookManager (tape)              ↓
                  └── allMids         ──→ PriceCache               RiskManager
                                                                        ↓
                                                                  REST /exchange ──→ Hyperliquid
```

**Pas de boucle timer.** Les stratégies sont invoquées à chaque update significatif du book (changement de spread, imbalance shift, trade tape). C'est la différence fondamentale avec l'approche chandelier.

**Principe de priorité des fills** : le channel WS `orderUpdates` est la source **primaire** pour détecter les fills. Le polling REST `userFillsByTime` est le fallback si le WS est coupé. Ne pas inverser cette priorité.

---

## 4. Phase 0 — Squelette et infra

### 4.1. Structure du projet

```bash
mkdir -p config exchange book strategy execution backtest data journal monitoring tests
touch main.py requirements.txt Dockerfile .env.example
```

### 4.2. Configuration (Pydantic Settings)

```python
# config/settings.py
from pydantic_settings import BaseSettings

class Settings(BaseSettings):
    # Exchange
    exchange: str = "hyperliquid"
    hl_api_url: str = "https://api.hyperliquid.xyz"
    hl_ws_url: str = "wss://api.hyperliquid.xyz/ws"
    hl_private_key: str = ""       # JAMAIS dans le code
    hl_wallet_address: str = ""
    
    # Coins
    coins: list[str] = ["BTC", "ETH", "SOL", "HYPE", "SUI"]
    
    # Risk
    max_open_positions: int = 5
    max_margin_usage_pct: float = 60.0
    max_loss_per_trade_pct: float = 3.0
    max_daily_loss_pct: float = 10.0
    max_drawdown_pct: float = 20.0
    drawdown_throttle_start: float = 7.0
    drawdown_throttle_severe: float = 12.0
    position_size_pct: float = 15.0
    min_position_size_usd: float = 11.0
    min_leverage: int = 5
    max_leverage: int = 50
    
    # Execution
    live_trading: bool = False
    use_post_only: bool = True      # ALO (Add Liquidity Only)
    pending_order_timeout_s: int = 30  # Cancel pending limit after N seconds
    
    # Microstructure
    book_depth_levels: int = 20     # Niveaux de depth à suivre
    imbalance_window: int = 50      # Trades pour calculer l'imbalance
    toxicity_threshold: float = 0.7 # Au-dessus = flow toxique, ne pas poster
    
    # Data recording
    record_l2: bool = True          # Enregistrer les données L2 pour backtest futur
    
    class Config:
        env_file = ".env"
        env_prefix = "TBOT_"
```

### 4.3. Logging structuré

```python
# Utiliser structlog avec sortie JSON
# Chaque log contient: timestamp, level, component, coin, event, latency_ms
# Fichier rotatif: logs/tbot-l2.jsonl (10 MB, 5 fichiers)
# PAS de print() — tout passe par le logger
```

### 4.4. Point d'entrée asyncio

```python
# main.py — structure cible
async def main():
    settings = Settings()
    
    # 1. Init exchange clients
    rest = RestClient(settings)
    signer = HyperliquidSigner(settings.hl_private_key)
    
    # 2. Load coin metadata (asset indices, sz decimals, max leverage)
    await rest.load_meta()
    
    # 3. Init book manager (un book par coin)
    book_mgr = BookManager(settings.coins)
    
    # 4. Init services
    risk_mgr = RiskManager(settings, rest)
    order_mgr = OrderManager(settings, rest, signer, risk_mgr)
    position_mgr = PositionManager(settings, rest, signer, order_mgr)
    
    # 5. Init strategies
    strategies = [
        MicroMeanRevert(settings, order_mgr, risk_mgr),
        ImbalanceFade(settings, order_mgr, risk_mgr),
        # ...
    ]
    
    # 6. Wire book updates → strategies
    book_mgr.on_update = lambda coin, book, features: [
        s.on_book_update(coin, book, features) for s in strategies
    ]
    
    # 7. Recover positions (si restart)
    await position_mgr.recover_positions()
    
    # 8. Start WS + lifecycle loop
    ws = WsClient(settings, book_mgr)
    await asyncio.gather(
        ws.run_forever(),                        # WS messages → book → strategies
        position_mgr.lifecycle_loop(),           # Check positions toutes les 5s
        order_mgr.pending_orders_loop(),         # Check fills toutes les 2s
        risk_mgr.daily_reset_loop(),             # Reset daily P&L à minuit UTC
    )
```

---

## 5. Phase 1 — Market data L2 (WebSocket)

### 5.1. WebSocket client (`ws_client.py`)

**Subscriptions Hyperliquid** :

```python
# Par coin :
{"method": "subscribe", "subscription": {"type": "l2Book", "coin": "BTC"}}
{"method": "subscribe", "subscription": {"type": "trades", "coin": "BTC"}}

# Global :
{"method": "subscribe", "subscription": {"type": "allMids"}}
```

**Points critiques (leçons t-bot)** :

| Risque | Protection |
|--------|------------|
| WS déconnecté silencieusement | Heartbeat toutes les 30s. Si pas de message reçu depuis 60s → reconnect forcé |
| Reconnect storm | Backoff exponentiel : 1s, 2s, 4s, 8s, max 30s |
| Message queue overflow | `asyncio.Queue(maxsize=10000)` — **NE PAS dropper les updates book** : un book stale est plus dangereux qu'une pause. Si la queue est pleine, throttler les stratégies sur les coins en retard (flag `book_stale[coin] = True`) plutôt que de dropper silencieusement. Dropper les trades tape est acceptable (moins critique). Logger un warning et mesurer le lag. |
| Parsing lent bloque le WS | Le WS thread ne fait que push dans la queue. Un consumer async séparé parse et dispatch |
| Données stale après reconnect | Après reconnect, request un snapshot REST full book avant de réappliquer les deltas |

**Structure d'un message `l2Book`** (Hyperliquid) :

```json
{
  "channel": "l2Book",
  "data": {
    "coin": "BTC",
    "levels": [
      [{"px": "65000.0", "sz": "1.2", "n": 3}, ...],   // bids
      [{"px": "65001.0", "sz": "0.8", "n": 2}, ...]    // asks
    ],
    "time": 1711900000123
  }
}
```

### 5.2. Book local (`order_book.py`)

```python
@dataclass(slots=True)
class OrderBook:
    coin: str
    bids: list[PriceLevel]       # Triés décroissant (best bid first)
    asks: list[PriceLevel]       # Triés croissant (best ask first)
    last_update_ms: int
    sequence: int                 # Pour détecter les gaps
    
    @property
    def best_bid(self) -> float: ...
    @property
    def best_ask(self) -> float: ...
    @property
    def mid_price(self) -> float: ...
    @property
    def spread(self) -> float: ...
    @property
    def spread_bps(self) -> float: ...
    
    def depth_at(self, bps: float) -> tuple[float, float]:
        """Volume cumulé bid/ask à N bps du mid"""
    
    def update_from_ws(self, levels_data: dict) -> None:
        """Met à jour le book depuis un message WS l2Book"""
```

### 5.3. Trade tape

Stocker les N derniers trades par coin dans un ring buffer (`collections.deque(maxlen=1000)`).

```python
@dataclass(slots=True)
class Trade:
    price: float
    size: float
    side: str          # "B" ou "S"
    timestamp_ms: int
```

### 5.4. Data recorder

**Enregistrer toutes les données brutes** pour pouvoir backtester les stratégies L2 plus tard.

```python
# Fichier par coin par jour : data/l2/BTC_2026-03-31.jsonl.gz
# Format : {"ts": 1711900000123, "type": "book", "bids": [...], "asks": [...]}
# Format : {"ts": 1711900000456, "type": "trade", "px": 65000.0, "sz": 0.1, "side": "B"}
#
# Compression gzip en streaming (pas de re-read, append-only)
# Rotation journalière automatique
```

**Estimations de volume** :
- ~20 coins × ~5 updates/sec au calme, **~50-100 updates/sec en session active** (BTC/ETH/SOL)
- Fourchette réaliste : ~50M-200M events/jour selon la volatilité
- ~100 bytes/event compressé = **5-20 GB/jour en session active**, ~1 GB/jour au calme
- Rétention : 30 jours = **30-600 GB** selon l'activité → prévoir 200 GB minimum, surveiller
- **Implication** : la `asyncio.Queue(maxsize=10000)` peut saturer sur BTC/SOL en période volatile — voir section 5.1 sur la gestion de la saturation

---

## 6. Phase 2 — Features microstructure

Calculées à chaque update du book et passées aux stratégies.

### 6.1. Features instantanées (book snapshot)

```python
@dataclass(slots=True)
class BookFeatures:
    # Spread
    spread_bps: float                # Spread en basis points
    spread_vs_avg: float             # Spread actuel / spread moyen roulant (> 1 = spread élargi)
    
    # Imbalance
    bid_ask_imbalance: float         # (bid_vol - ask_vol) / (bid_vol + ask_vol), [-1, +1]
    top_5_imbalance: float           # Idem sur les 5 premiers niveaux
    weighted_imbalance: float        # Pondéré par distance au mid (niveaux proches = plus de poids)
    
    # Depth
    bid_depth_10bps: float           # Volume cumulé bid à 10 bps du mid
    ask_depth_10bps: float           # Volume cumulé ask à 10 bps du mid
    depth_ratio: float               # bid_depth / ask_depth
    thin_side: str | None            # "bid" ou "ask" ou None si équilibré
    
    # Micro-price
    micro_price: float               # prix pondéré par les quantités au meilleur bid/ask
    micro_price_vs_mid: float        # micro_price - mid, en bps (>0 = pression acheteuse)
    
    # VAMP (Volume-Adjusted Mid Price)
    vamp: float                      # mid pondéré par depth sur N niveaux
    vamp_signal: float               # vamp - mid, normalisé par ATR (micro-directionnel)
```

### 6.2. Features temporelles (fenêtre roulante)

```python
@dataclass(slots=True) 
class FlowFeatures:
    # Trade flow (fenêtre roulante de 30s-120s)
    buy_volume: float                # Volume acheté dans la fenêtre
    sell_volume: float               # Volume vendu dans la fenêtre
    net_flow: float                  # buy - sell
    flow_imbalance: float            # (buy - sell) / (buy + sell), [-1, +1]
    trade_intensity: float           # Nombre de trades / seconde
    avg_trade_size: float            # Taille moyenne des trades (gros = institutional)
    large_trade_ratio: float         # % de trades > 2× la taille moyenne
    
    # Toxicity (adverse selection proxy)
    # Proportion de trades maker qui partent immédiatement dans le mauvais sens
    fill_toxicity: float             # 0-1, calculé sur les 100 derniers trades
    
    # Volatility micro
    realized_vol_1m: float           # Vol réalisée 1 min (mid-to-mid)
    realized_vol_5m: float           # Vol réalisée 5 min
    vol_ratio: float                 # vol_1m / vol_5m (>1 = accélération)
```

### 6.3. Formules clés

**Micro-price** :
$$P_{micro} = P_{ask} \times \frac{Q_{bid}}{Q_{bid} + Q_{ask}} + P_{bid} \times \frac{Q_{ask}}{Q_{bid} + Q_{ask}}$$

Où $Q_{bid}$, $Q_{ask}$ = quantités au meilleur niveau. Si le bid est plus épais que l'ask, le micro-price tire vers l'ask (direction probable du prochain mouvement).

**VAMP (Volume-Adjusted Mid Price)** sur $N$ niveaux :
$$VAMP = \frac{\sum_{i=1}^{N} (P_i^{bid} \times Q_i^{ask} + P_i^{ask} \times Q_i^{bid})}{\sum_{i=1}^{N} (Q_i^{ask} + Q_i^{bid})}$$

**Order Flow Imbalance (OFI)** roulant :
$$OFI_t = \frac{\sum_{k} V_k^{buy} - \sum_{k} V_k^{sell}}{\sum_{k} V_k^{buy} + \sum_{k} V_k^{sell}}$$

Calculé sur une fenêtre temporelle (pas un nombre fixe de trades) pour normaliser les périodes calmes vs actives.

**Fill toxicity** (proxy d'adverse selection) :
Pour chaque trade récent, regarder si le mid-price a bougé dans la direction du trade dans les 5s suivantes. Si > 70% des trades sont "toxiques" (le prix continue dans la direction), le flow est informatif → ne PAS poster de l'autre côté.

> **⚠ Délai inhérent** : la toxicity calculée à T est disponible en réalité à T+5s (on ne connaît le mid T+5s qu'à T+5s). La valeur utilisée dans `on_book_update` a donc **5 secondes de retard**. C'est acceptable pour un filtre binaire (poster / ne pas poster), mais ne pas l'utiliser comme signal directionnel précis. En complément, utiliser un **proxy instantané** de toxicity : proportion de trades du dernier clip qui ont consommé le meilleur niveau vs. trades internes (pas de lookahead).

---

## 7. Phase 3 — Stratégies

### 7.1. Interface commune

```python
class Strategy(ABC):
    @abstractmethod
    async def on_book_update(self, coin: str, book: OrderBook, 
                              book_features: BookFeatures, flow_features: FlowFeatures) -> Signal | None:
        """Appelé à chaque update significatif du book. Retourne un signal ou None."""
    
    @abstractmethod
    async def on_trade(self, coin: str, trade: Trade) -> Signal | None:
        """Appelé à chaque trade sur le tape. Optionnel (peut return None)."""
    
    @property
    @abstractmethod
    def name(self) -> str: ...
```

### 7.2. Stratégie 1 : Micro Mean Reversion (`micro_mean_revert.py`)

**Logique** : quand le micro-price / VAMP dévie significativement du mid dans une direction, et que le flow s'essouffle (imbalance diminue), poster un order de l'autre côté pour capturer le retour au mid.

**Conditions d'entrée** :
- `|vamp_signal|` > seuil (ex: 2 bps)
- `flow_imbalance` en train de se retourner (dérivée change de signe)
- `fill_toxicity` < 0.5 (pas de flow toxique)
- `spread_bps` < 5 bps (assez de liquidité pour capturer)
- Direction : SHORT si vamp > mid (prix sur-évalué), LONG si vamp < mid

**Sortie** :
- TP : retour au mid (ou VAMP neutre) → quelques bps
- SL : 2× le TP en bps (asymétrique, le win rate doit compenser)
- Timeout : 60 secondes → cancel tout

**Exécution** : Post-only à 1 tick du best bid/ask (maker).

### 7.3. Stratégie 2 : Imbalance Fade (`imbalance_fade.py`)

**Logique** : quand l'imbalance bid/ask est extrême (> 0.7) mais que les trades ne poussent pas le prix (absorption), poster dans la direction contraire.

**Conditions d'entrée** :
- `top_5_imbalance` > 0.7 (massif)
- Mais le mid-price n'a PAS bougé significativement (< 1 bps en 10s)
- Volume de trades élevé → quelqu'un absorbe
- `fill_toxicity` < 0.4 (flow non-informatif)

**Logique** : l'imbalance est un mirage — les orders affichés ne se traduisent pas en mouvement de prix → ils seront retirés → le prix revient.

### 7.4. Stratégie 3 : Breakout confirmé par flow (`breakout_flow.py`)

**Logique** : breakout classique (nouveau high/low N secondes) MAIS uniquement si confirmé par :
- `flow_imbalance` > 0.5 dans la direction du breakout
- `trade_intensity` > 2× la moyenne (urgence)
- `depth_ratio` déséquilibré (la résistance a été consommée)
- `fill_toxicity` > 0.6 (flow informatif = smart money pousse)

**Exécution** : IOC (taker) car on veut capturer le momentum. Seule strat qui utilise le taker — le trade doit avoir un edge brut > 9 bps pour compenser les frais round-trip.

### 7.5. Stratégie 4 : Pullback directionnel maker (`pullback_maker.py`)

**Logique** : après un mouvement directionnel (mid shift > N bps en 60s), attendre un pullback de 30-50% et poster un limit dans la direction initiale.

**Conditions d'entrée** :
- Mid-price a bougé de > X bps dans les 60 dernières secondes (momentum établi)
- Un pullback de 30-50% est en cours (prix revient vers le bid si mouvement haussier)
- `flow_imbalance` toujours positif (le flux de fond n'a pas changé)
- `fill_toxicity` < 0.5

**Exécution** : Post-only au prix du pullback (maker). Si pas fill en 30s → cancel.

### 7.6. Scoring simplifié

Pas de scoring à 10 composantes comme tbot-scalp. Chaque stratégie retourne un `Signal` avec un `confidence: float` normalisé 0-1. Le confidence est le produit des conditions satisfaites (chaque condition contribue un facteur 0-1).

```python
@dataclass
class Signal:
    coin: str
    direction: str              # "LONG" / "SHORT"
    strategy: str
    confidence: float           # 0.0 - 1.0
    entry_price: float          # Prix cible limit
    stop_loss: float
    take_profit: float
    post_only: bool             # True = maker, False = IOC taker
    max_fill_wait_s: int        # Timeout avant cancel
    timestamp_ms: int
```

**Seuil d'exécution** : `confidence >= 0.6` (un seul seuil, pas de "Very Confident" vs "Risky").

### 7.7. Arbitrage des signaux concurrents sur le même coin

Plusieurs stratégies peuvent générer des signaux sur le même coin au même tick. Règles d'arbitrage :

| Cas | Règle |
|-----|-------|
| Même coin, même direction, stratégies différentes | Prendre le signal de **plus haute confidence**. Ne pas doubler la position. |
| Même coin, directions opposées | **Ne pas entrer**. Signaux contradictoires = incertitude → skip. |
| Même coin, position déjà ouverte | **Ignorer tous les nouveaux signaux sur ce coin** jusqu'à fermeture (pas de pyramiding). |
| Coins différents, corrélés (ex: SOL + AVAX tous deux LONG) | Autorisé mais compté dans le drawdown BTC-correlation (voir Phase 5). |

Cette règle doit être centralisée dans `OrderManager.process_signals()` avant tout envoi d'ordre.

---

## 8. Phase 4 — Exécution (ordres maker)

### 8.1. Post-Only / ALO (Add Liquidity Only)

**Différence critique avec tbot-scalp** : tbot-scalp utilise des GTC limit qui peuvent cross le book et devenir taker. tbot-scalp-l2 doit utiliser le mode ALO (implicite sur Hyperliquid si le prix est au-delà du best bid/ask).

**Payload Hyperliquid pour un ordre limit GTC (maker intent)** :
```json
{
  "action": {
    "type": "order",
    "orders": [{
      "a": 0,          // asset index
      "b": true,       // is buy
      "p": "64999.0",  // prix AU NIVEAU du best bid (ou mieux) pour rester maker
      "s": "0.001",    // quantity
      "r": false,      // reduce only
      "t": {"limit": {"tif": "Alo"}}   // Add Liquidity Only — rejeté si cross
    }],
    "grouping": "na"
  }
}
```

> **Note Hyperliquid** : les batches ALO-only sont **priorisés** par le matching engine.
> Séparer les batches ALO des IOC/GTC pour bénéficier de cette priorité.

### 8.2. Placement intelligent

```python
async def place_maker_order(self, signal: Signal) -> OrderResult:
    book = self.book_manager.get_book(signal.coin)
    
    if signal.direction == "LONG":
        # Poster au best bid (ou 1 tick au-dessus si spread > 2 ticks)
        price = book.best_bid
        if book.spread > 2 * tick_size:
            price = book.best_bid + tick_size  # Improve pour être en tête de queue
    else:
        price = book.best_ask
        if book.spread > 2 * tick_size:
            price = book.best_ask - tick_size
    
    # Vérification anti-cross : si le prix améliore le spread, on risque de fill en taker
    # → ajuster au best bid/ask exact (pas mieux)
    
    result = await self.rest.place_order(
        coin=signal.coin,
        is_buy=(signal.direction == "LONG"),
        price=price,
        size=signal.quantity,
        tif="Alo",         # IMPORTANT: ALO pas GTC
        reduce_only=False,
    )
    return result
```

### 8.3. Suivi des ordres pending

**Deux chemins complémentaires :**

**Chemin primaire — WS `orderUpdates`** (sub-seconde) :
```python
# Subscription ajoutée au démarrage
{"method": "subscribe", "subscription": {"type": "orderUpdates", "user": "<wallet>"}}

# Callback WS → fill immédiat sans polling
async def on_order_update(self, msg):
    oid = msg["oid"]
    status = msg["status"]  # "filled", "cancelled", "open"
    if status == "filled" and oid in self.pending:
        fill_price = msg.get("avgPx") or msg.get("px")
        await self.on_fill(self.pending[oid], fill_price)
        del self.pending[oid]
    elif status == "cancelled" and oid in self.pending:
        del self.pending[oid]
```

**Chemin fallback — boucle polling toutes les 5s** (si WS coupé ou message manqué) :
```python
async def pending_orders_loop(self):
    while True:
        await asyncio.sleep(5)   # Fallback seulement, le WS est primaire
        for oid, order in list(self.pending.items()):
            elapsed = time.time() - order.placed_at
            if elapsed > order.max_wait_s:
                await self.cancel_order(oid)
                del self.pending[oid]
```

**Logique de cancel/amend pilotée par le book (dans `on_book_update`)** :
```python
# IMPORTANT : déclenché par les updates du book, pas par un timer
async def on_book_update(self, coin, book, features):
    for oid, order in list(self.pending.items()):
        if order.coin != coin:
            continue

        # 1. Flow toxique → cancel immédiat
        if features.fill_toxicity > self.settings.toxicity_threshold:
            await self.cancel_order(oid)
            del self.pending[oid]
            continue

        # 2. Prix plus compétitif possible → AMEND (pas cancel+replace)
        #    Hyperliquid supporte la modification de prix sans cancel
        new_price = self._compute_competitive_price(order, book)
        if new_price and abs(new_price - order.price) > tick_size:
            amended = await self.amend_order(oid, new_price)
            if amended:
                order.price = new_price
            # Si amend échoue (ordre déjà fillé entre-temps) → on_fill sera
            # déclenché par le WS orderUpdates

        # 3. Signal de la stratégie s'est retourné (stale quote)
        signal_still_valid = await self.strategy.is_signal_valid(order.signal, book, features)
        if not signal_still_valid:
            await self.cancel_order(oid)
            del self.pending[oid]
```

> **⚠ Amend vs Cancel+Replace** : utiliser `amend_order` (modification de prix) de préférence à cancel+replace pour le suivi de prix. Cancel+replace = 2 appels API + perte de position dans la queue. Amend = 1 appel API + conservation de la queue position. Utiliser cancel+replace uniquement si la taille change également.

### 8.4. Fill detection

**WS `orderUpdates` = source primaire** (voir 8.3). Le REST `userFillsByTime` est le fallback.

```python
# Le vrai prix de fill vient de avgPx dans le message WS ou de userFillsByTime
# JAMAIS utiliser le prix théorique du signal comme prix d'entrée (bug t-bot 03-26)
fill_price = ws_msg.get("avgPx") or await self.rest.get_fill_price(oid)
```

### 8.5. Annulation rapide (anti-adverse selection)

**Règle critique** : si `fill_toxicity` dépasse le seuil pendant qu'on a un ordre resting → **cancel immédiat**.

```python
# Dans la callback on_book_update, pour chaque pending order :
if flow_features.fill_toxicity > self.settings.toxicity_threshold:
    await self.cancel_all_pending(coin)
    log.warning("Toxic flow detected", coin=coin, toxicity=flow_features.fill_toxicity)
```

C'est la protection principale contre l'adverse selection mentionnée dans l'analyse.

---

## 9. Phase 5 — Risk management

### 9.1. Règles portées de tbot-scalp (prouvées en live)

| Règle | Valeur | Source (bug corrigé) |
|-------|--------|---------------------|
| Drawdown calculé sur **equity** (pas balance) | oui | t-bot bug #12 |
| Daily loss limit | 10% | — |
| Drawdown circuit breaker | 20% | — |
| Drawdown throttle start | 7% → max positions ÷ 2 | — |
| Drawdown throttle severe | 12% → 1 position max | — |
| Max margin usage | 60% | — |
| Max loss per trade | 3% du portfolio | — |
| Pas de doublon coin (toutes TF confondues) | oui | simplifié vs t-bot (pas de notion TF ici) |
| Cooldown après fermeture | 60 secondes (court, c'est du scalp) | — |
| Contrary signal auto-close | 3 signaux opposés → fermeture | t-bot feature |

### 9.2. Règles spécifiques L2

| Règle | Seuil | Raison |
|-------|-------|--------|
| Spread trop large → pas de trade | > 10 bps | Pas de place pour l'edge maker |
| Depth trop faible → pas de trade | < $5000 à 10 bps | Slippage SL trop élevé |
| Toxicity élevée → pas de posting | > 0.7 | Adverse selection > edge |
| Volatilité micro explosive → pas de trade | vol_ratio > 3.0 | Marché en mode discovery, pas de reversion |
| Corrélation BTC → réduction exposition | si BTC drawdown > 5% intraday | Tous les alts suivent |

### 9.3. Gestion de la corrélation inter-coin

5 positions LONG simultanées sur des alts (SOL, HYPE, SUI, DOGE, AVAX) pendant un dump BTC = **risque concentré, pas diversifié**. Les alts bougent ensemble dans 80%+ des cas.

Règles concrètes :

```python
# Dans RiskManager.validate_signal()

# 1. Compter les positions directionnelles ouvertes
long_count = sum(1 for p in open_positions if p.direction == "LONG")
short_count = sum(1 for p in open_positions if p.direction == "SHORT")

# 2. Limiter le biais directionnel net (max 3 positions dans la même direction)
MAX_DIRECTIONAL_BIAS = 3
if signal.direction == "LONG" and long_count >= MAX_DIRECTIONAL_BIAS:
    return ["Directional bias: already {} LONGs open".format(long_count)]
if signal.direction == "SHORT" and short_count >= MAX_DIRECTIONAL_BIAS:
    return ["Directional bias: already {} SHORTs open".format(short_count)]

# 3. Si BTC a fait -3% en 15min → bloquer nouveaux LONGs sur tous les alts
if btc_move_15m < -0.03 and signal.direction == "LONG" and signal.coin != "BTC":
    return ["BTC sell-off in progress — no new LONG on alts"]
```

### 9.3. Position sizing (identique tbot-scalp)

```python
pos_size_usd = balance * position_size_pct / 100
quantity = pos_size_usd * leverage / entry_price

# Leverage risk-based : target maxLossPerTrade% of portfolio
target_leverage = (max_loss_per_trade_pct * 100) / (sl_distance_pct * position_size_pct)
leverage = clamp(target_leverage, min_leverage, min(max_leverage, coin_max_leverage))
```

### 9.4. Guard equity (leçon tbot-scalp 2026-03-31)

```python
# JAMAIS faire 2 appels API séparés pour total et hold spot
# → un seul appel spotClearinghouseState, parser total et hold ensemble
# → cross-validation : si hold=0 mais marginUsed > 0, c'est une race condition API
#   → fallback conservateur (ignorer unrealized PnL plutôt que double-compter)

# Guard anti-spike : si equity saute de > 5% en 1 cycle, ignorer (artefact API)
# Le peakEquity ne doit JAMAIS être basé sur une valeur non-validée
```

---

## 10. Phase 6 — Position lifecycle

### 10.1. États d'une position

```
SIGNAL → PENDING_FILL → OPEN → [BREAK_EVEN] → [TRAILING] → CLOSED
                ↓                                              ↑
           CANCELLED (timeout / toxic flow / cancel)     (TP/SL/TIMEOUT/MANUAL/CONTRARY)
```

### 10.2. TP/SL trigger orders (leçon t-bot)

Dès qu'un fill est confirmé → poser les trigger orders TP/SL sur l'exchange.

```python
async def on_fill(self, order: PendingOrder, fill_price: float):
    # 1. Recalculer TP/SL proportionnellement si fill_price diffère de l'entry théorique
    #    (leçon t-bot bug #2 : prix TP/SL périmés)
    ratio = fill_price / order.signal.entry_price
    adjusted_sl = order.signal.stop_loss * ratio
    adjusted_tp = order.signal.take_profit * ratio
    
    # 2. Poser les triggers (SL en trigger-limit, TP en GTC limit)
    tp_oid = await self.rest.place_trigger_order(
        coin=order.coin, is_buy=not order.is_buy,
        price=adjusted_tp, size=order.quantity,
        tif="Gtc", reduce_only=True, trigger_type=None  # TP = simple limit
    )
    sl_oid = await self.rest.place_trigger_order(
        coin=order.coin, is_buy=not order.is_buy,
        price=adjusted_sl, size=order.quantity,
        tif=None, reduce_only=True,
        trigger_type="sl", trigger_px=adjusted_sl
    )
    
    # 3. Tracker la position
    position = OpenPosition(
        coin=order.coin,
        direction=order.signal.direction,
        entry_price=fill_price,
        stop_loss=adjusted_sl,
        take_profit=adjusted_tp,
        original_stop_loss=adjusted_sl,
        tp_trigger_oid=tp_oid,
        sl_trigger_oid=sl_oid,
        # ... etc
    )
    self.positions[order.coin] = position
```

### 10.3. Break-even et trailing stop

**Break-even** (leçon t-bot bug #7 et #9) :
```python
# Quand le prix atteint X% du TP → déplacer SL au prix d'entrée
# IMPORTANT : utiliser AMEND si seul le prix SL change (pas la taille)
#             → conservation de la queue position + 1 seul appel API
#             → utiliser cancel+replace uniquement si le trigger type change aussi
# IMPORTANT : stocker original_stop_loss séparément (ne pas l'écraser)
# IMPORTANT : au restart, détecter le BE par comparaison SL/entry (< 0.2% = BE appliqué)
```

**SL progressif par paliers** (leçon t-bot 2026-03-27) :
```python
# Après le break-even, le SL monte par paliers :
# TP progress >= 65% → SL = entry + 25% du profit
# TP progress >= 80% → SL = entry + 50% du profit
# Chaque palier = cancel + replace du trigger order sur l'exchange
```

### 10.4. Sync avec l'exchange (leçon t-bot bug #13)

```python
async def sync_with_exchange(self):
    try:
        exchange_positions = await self.rest.get_open_positions()
    except Exception as e:
        # JAMAIS interpréter une erreur API comme "0 positions"
        # (leçon t-bot bug #13 : faux SL_HIT sur 429)
        log.error("Sync failed, skipping", error=str(e))
        return
    
    # Safety guard : si exchange retourne 0 positions mais on en tracke > 0
    # ET qu'il n'y a pas de fills récents pour ces coins → probablement erreur API
    if not exchange_positions and self.positions:
        recent_fills = await self.rest.get_recent_close_fills(since_ms=7200_000)
        tracked_coins = set(self.positions.keys())
        closed_coins = {f.coin for f in recent_fills if f.coin in tracked_coins}
        
        if not closed_coins:
            log.warning("Exchange returned 0 positions but we track %d — skipping sync",
                       len(self.positions))
            return
        
        # Certains coins ont des fills → fermer ceux-là, garder les autres
        for coin in closed_coins:
            fill_price = recent_fills[coin].price
            await self.close_position(coin, fill_price, "EXCHANGE_CLOSED")
    
    # Détection positions orphelines (exchange mais pas trackées → recover)
    local_coins = set(self.positions.keys())
    exchange_coins = {p.coin for p in exchange_positions}
    orphans = exchange_coins - local_coins
    if orphans:
        log.warning("Orphan positions detected: %s — recovering", orphans)
        await self.recover_positions()
```

### 10.5. Stale quote — cancel quand la stratégie se retourne

Un ordre resting posté 30 secondes ago au best bid peut être fortement adverse si le contexte microstructure a changé depuis. Le timeout `max_fill_wait_s` couvre le cas général, mais il faut aussi cancel explicitement si la stratégie **ne confirme plus** le signal :

```python
# Appelé depuis on_book_update pour chaque pending order
async def is_signal_still_valid(self, order: PendingOrder, book: OrderBook,
                                 features: BookFeatures) -> bool:
    """La stratégie source confirme-t-elle encore ce signal ?"""
    strategy = self.strategies[order.signal.strategy]
    # Recalculer le signal avec les nouvelles features
    new_signal = await strategy.on_book_update(order.coin, book, features)
    # Si la stratégie ne génère plus de signal dans la même direction → stale
    if new_signal is None or new_signal.direction != order.signal.direction:
        return False
    return True
```

**Cas pratique** : bot a posté un LONG sur SOL au best bid. 15 secondes plus tard, l'imbalance s'inverse (pression sell massive) et la toxicity monte. La stratégie ne génèrerait plus ce signal → cancel immédiat, ne pas attendre le timeout de 30s.

### 10.6. Nettoyage des trigger orders orphelins (leçon t-bot 2026-03-25)

```python
# Après recovery, vérifier que chaque coin n'a que 2 triggers (1 TP + 1 SL)
# Si d'autres triggers existent (vestiges d'un ancien crash) → les annuler
# Utilise frontendOpenOrders pour lister tous les triggers d'un coin
```

---

## 11. Phase 7 — Backtest

### 11.1. Problème du backtest L2

Le backtest sur données chandelier est fondamentalement trompeur pour un bot L2 :
- Pas de book → impossible de calculer les features microstructure
- Pas de fill probability → un ordre maker n'est pas forcément fillé
- Pas de queue position → en backtest on est toujours premier en queue

**Solution** : backtest en 2 niveaux.

### 11.2. Niveau 1 : Replay de données enregistrées

Une fois que le recorder (Phase 1) a accumulé des données, on peut faire un **replay tick-by-tick** :

```python
class ReplayEngine:
    async def replay(self, data_files: list[str]):
        """Rejoue les événements L2/trades dans l'ordre chronologique"""
        for event in self.load_events(data_files):
            if event.type == "book":
                self.sim_book.update(event)
                features = self.feature_engine.compute(self.sim_book)
                for strategy in self.strategies:
                    signal = strategy.on_book_update(event.coin, self.sim_book, features)
                    if signal:
                        self.sim_execution.process(signal, self.sim_book)
            elif event.type == "trade":
                for strategy in self.strategies:
                    strategy.on_trade(event.coin, event.trade)
            
            self.sim_execution.check_fills(self.sim_book)
```

### 11.3. Niveau 2 : Modèle de fill probabiliste

En backtest, on ne peut pas juste supposer que chaque limite est fillée. Il faut un modèle :

```python
class SimExecution:
    def should_fill(self, order, book, elapsed_s: float) -> bool:
        """Modèle de fill basé sur queue position et flow"""
        # 1. Le prix doit atteindre notre ordre
        if order.is_buy and book.best_ask > order.price:
            return False
        
        # 2. Queue position : on n'est pas premier
        #    Probabilité = volume tradé au prix / volume total au prix
        vol_traded_at_price = self.get_volume_traded_at(order.price, since=order.placed_at)
        vol_queued = self.get_depth_at(order.price, at=order.placed_at)
        
        # On suppose qu'on est au milieu de la queue
        fill_prob = min(1.0, vol_traded_at_price / (vol_queued * 0.5))
        
        # 3. Biaisé par toxicity : si le fill arrive, c'est probablement adverse
        #    → appliquer un "winner's curse" discount
        adverse_adjust = 1.0 - self.toxicity * 0.3
        
        return random.random() < fill_prob * adverse_adjust
```

### 11.4. Backtest chandelier (fallback)

Pour la phase de développement initiale (avant d'avoir des données L2 enregistrées), un backtest simplifié sur données OHLCV reste utile, avec les mêmes corrections que tbot-scalp :
- Frais : `taker_fee_pct` et `maker_fee_pct` appliqués
- Slippage entrée : 0.03%, sortie : 0.04%
- Fill rate maker : 70% (pas 100%) — **valeur arbitraire à calibrer sur les premières semaines live**. En marché trending, fill rate ALO peut tomber à 20-30% (le prix ne revient pas). En marché ranging, 60-80% est réaliste.
- Backtest Binance pour l'historique 1m/3m (au-delà des 5000 candles Hyperliquid)

---

## 12. Phase 8 — Dashboard & monitoring

### 12.1. FastAPI + SSE

```python
# monitoring/dashboard.py
from fastapi import FastAPI
from sse_starlette.sse import EventSourceResponse

app = FastAPI()

@app.get("/api/state")
async def state():
    return {
        "positions": [...],
        "pending_orders": [...],
        "risk": {"equity": ..., "drawdown": ..., "daily_pnl": ...},
        "books": {coin: {"spread": ..., "imbalance": ..., "toxicity": ...} for coin in coins},
        "metrics": {"avg_latency_ms": ..., "fill_rate": ..., "maker_ratio": ...}
    }

@app.get("/api/stream")
async def stream():
    """SSE stream pour le dashboard"""
    async def generator():
        while True:
            yield {"data": json.dumps(get_current_state())}
            await asyncio.sleep(1)
    return EventSourceResponse(generator())
```

### 12.2. Métriques spécifiques L2

| Métrique | Description |
|----------|-------------|
| `avg_fill_latency_ms` | Temps moyen entre placement et fill |
| `maker_fill_rate` | % des ordres ALO qui sont fillés (vs cancelled/timeout) |
| `adverse_selection_rate` | % des fills suivis d'un mouvement défavorable > 2 bps en 5s |
| `spread_capture_bps` | Bps effectivement capturés (fill → close) net de frais |
| `cancel_rate` | % des ordres annulés (trop élevé = trop agressif, trop bas = trop passif) |
| `toxicity_cancel_rate` | % des annulations causées par la détection de flow toxique |

---

## 13. Phase 9 — Déploiement & opérations

### 13.1. Docker

```dockerfile
FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY . .

# Volumes pour la persistance
VOLUME ["/app/data", "/app/journal", "/app/history", "/app/logs"]

CMD ["python", "-m", "uvloop", "main.py"]
```

### 13.2. docker-compose

```yaml
services:
  tbot-l2:
    build: .
    ports:
      - "9092:9092"    # Dashboard
    volumes:
      - ./data:/app/data
      - ./journal:/app/journal
      - ./history:/app/history
      - ./logs:/app/logs
    env_file: .env
    restart: unless-stopped
    # Pas de healthcheck HTTP au départ — juste le process
```

### 13.3. Opérations

| Action | Commande |
|--------|----------|
| Dry-run (mode observation) | `TBOT_LIVE_TRADING=false python main.py` |
| Live | `TBOT_LIVE_TRADING=true python main.py` |
| Backtest replay | `python -m backtest.replay_engine --data data/l2/` |
| Exporter le journal | `cat journal/trades_*.jsonl \| python -m journal.export` |

---

## 14. Leçons des bugs t-bot / tbot-scalp

Catalogue complet des bugs corrigés sur t-bot et tbot-scalp, avec la protection correspondante dans tbot-scalp-l2.

### 14.1. Bugs d'exécution

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #1 | `checkNaturalClose` ne fermait pas sur l'exchange | Positions fantômes | `close_position()` appelle toujours l'exchange en live |
| t-bot #2 | Prix TP/SL non recalculés après fill | TP/SL décalés | Recalcul proportionnel au fill price systématique |
| t-bot #7 | Break-even SL pas mis à jour sur l'exchange | Ancien SL actif après restart | Cancel + replace du trigger order. Stocker `original_stop_loss` séparément |
| t-bot #9 | Break-even perdu au restart | SL reset à l'original | Détection automatique par comparaison SL/entry (< 0.2%) |
| t-bot #10 | Signaux récents avec prix périmés | Entry à un prix obsolète | Non applicable (le signal vient du book temps réel, pas d'une candle passée) |
| scalp 03-31 | Exit price théorique au lieu du vrai fill | PnL incorrect dans le journal | Parser `avgPx` de la réponse exchange et `userFillsByTime` comme fallback |

### 14.2. Bugs de communication exchange

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #4 | Réponses trigger orders ignorées | Erreurs silencieuses | Check `"err"` dans la réponse de chaque trigger order |
| t-bot #13 | `getOpenPositions()` retournait `[]` sur 429 | Faux SL_HIT → positions orphelines | Propager l'exception. Safety guard si 0 positions exchange + N positions trackées. Réconciliation auto |
| t-bot #14 | Thread scheduler mort (RestTemplate sans timeout) | Bot brain-dead | Timeout 30s sur TOUTES les requêtes HTTP. Pas de thread unique critique (`asyncio` résout ça nativement) |
| t-bot #15 | Rate limiter 3× trop permissif | 429 systématiques | Poids réels : candleSnapshot=20+, meta/openOrders=20, allMids=2. Budget calculé dynamiquement |
| scalp 03-25 | Rate limiter non partagé entre services | Budget dépassé | Un seul `RateLimiter` partagé (naturel en Python : une seule instance) |
| scalp 03-25 | Trigger orders orphelins après crash | Doublons TP/SL | Nettoyage au startup : lister tous les triggers, annuler ceux non trackés |

### 14.3. Bugs de calcul de risque

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #12 | Drawdown calculé sur `availableBalance` au lieu d'`equity` | Faux drawdown → throttle | Toujours utiliser equity pour le drawdown |
| scalp 03-31 | `getEquity()` race condition (2 appels API séparés pour spot total/hold) | Peak equity gonflé → faux drawdown 13% | **Un seul appel** `spotClearinghouseState`, parser total et hold ensemble |
| scalp 03-31 | Double-counting sur comptes unifiés (Portfolio Margin) | Equity surestimée de ~15% → circuit breaker | `equity = spotBalance (total - hold) + accountValue`. Cross-validation si hold=0 mais margin > 0 |
| scalp 03-31 | Safety guard sync trop agressif | Positions réellement fermées restaient "stuck" | Vérifier `getRecentCloseFillPrices()` avant d'ignorer le sync |

### 14.4. Bugs de données / config

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #3 | Hardcodé Kraken dans sync | Crash | Un seul exchange (Hyperliquid), pas d'abstraction multi-exchange |
| t-bot #8 | Leverage identique pour tous les coins | Levier trop élevé sur small caps | `effectiveMaxLeverage = min(config, coin_max_leverage)` — chargé via `meta` |
| t-bot #11 | xyz assets pas chargés dans `refreshMeta()` | xyz rejetés en live | Charger les deux univers (standard offset=0, xyz offset=110000). Passer `"dex":"xyz"` partout |
| t-bot 03-26 | Exit price = prix théorique SL/TP au lieu du vrai fill | Stats faussées | `userFillsByTime` pour récupérer le vrai prix de sortie |
| t-bot 03-27 | Risk tiers pénalisaient les gros portefeuilles | 23 trades skippés dont 13 winners | Tier unique flat — risque identique en % quelle que soit la taille |

### 14.5. Bugs de backtest

| # | Bug | Impact | Protection L2 |
|---|-----|--------|----------------|
| t-bot #6 | Backtest divergeait du live (pas de règles de risque, pas de slippage) | Faux positifs | Simuler les frais, slippage, fill rate, et toutes les règles risk |
| scalp 03-30 | Backtest sans frais ni slippage donnait des ROI aberrants (240-3176%) | Illusion de profitabilité | Frais `taker_fee` + `exit_slippage` intégrés dès le jour 1. Fill rate maker < 100% |

### 14.6. Erreurs d'architecture

| Problème | Impact | Design L2 |
|----------|--------|-----------|
| `RestTemplate` sans timeout (défaut Java) | Appels bloquants infinis | `aiohttp` avec `timeout=ClientTimeout(total=30)` par défaut |
| Spring scheduler pool size = 1 (défaut) | 1 thread mort = tout meurt | `asyncio` : single event loop, les tâches sont indépendantes et ne bloquent pas |
| `try-catch` manquant sur les tâches planifiées | Crash silencieux du scheduler | Wrapper générique sur chaque coroutine longue : log + continue |
| Données stale non détectées (candles vieilles) | Signaux sur des données périmées | Le book temps réel est toujours frais. Heartbeat WS pour détecter la staleté |
| Pas de guard sur equity spike | `peakEquity` corrompu → faux drawdown | Guard si equity jump > 5% en 1 cycle → ignorer et logger |

---

## 15. Checklist pré-live

### Avant d'activer `live_trading=true`

- [ ] **Dry-run stable 48h** sans crash, reconnect WS fonctionnel
- [ ] **Rate limiter validé** : 0 erreurs 429 sur 24h
- [ ] **Equity calculation validée** : comparer `getEquity()` avec le dashboard Hyperliquid manuellement (5 checks)
- [ ] **Maker fill rate mesurée** : > 40% sinon les strats ne sont pas viables
- [ ] **Fill toxicity mesurée** : < 60% sinon l'adverse selection mange l'edge
- [ ] **Recovery testé** : kill process → restart → positions récupérées, SL/TP toujours posés, pas de doublons
- [ ] **Circuit breaker testé** : simuler un drawdown 20% → tous les ordres annulés, aucun nouveau trade
- [ ] **Trigger order placement validé** : poser un TP/SL, vérifier sur le dashboard HL, cancel → vérifier qu'il disparaît
- [ ] **Annulation rapide testée** : placer un ALO → cancel dans les 5s → vérifier qu'aucun fill partiel n'a créé de position orpheline
- [ ] **Journal/history écriture OK** : vérifier que les JSONL sont écrits après chaque trade, parseable
- [ ] **Monitoring alertes** : webhook Discord/Telegram pour les events critiques (circuit breaker, crash, position orpheline)

### Monitoring continu post-live

- [ ] Spread moyen capturé > frais round-trip (sinon le bot perd de l'argent structurellement)
- [ ] Win rate > 50% (nécessaire pour un bot maker avec edge small)
- [ ] Max drawdown < 10% sur les 7 premiers jours
- [ ] Pas de positions orphelines (sync exchange OK)
- [ ] Data recorder fonctionne (fichiers L2 générés quotidiennement)

---

## Ordre d'implémentation recommandé

```
Phase 0 (squelette)         → 1 jour
Phase 1 (WS + book)         → 4-6 jours  (reconnect stable, gaps de séquence, heartbeat, tests)
Phase 2 (features)          → 2-3 jours
Phase 4 (exécution maker)   → 4-6 jours  (ALO, amend, orderUpdates WS, cancel logic, tests)
Phase 5 (risk management)   → 2-3 jours
Phase 6 (position lifecycle)→ 4-6 jours  (recovery, orphans, break-even, trailing — source de 9/15 bugs t-bot)
Phase 3 (stratégies)        → 3-5 jours  (itératif, une à la fois, après observation)
Phase 7 (backtest replay)   → 3-5 jours
Phase 8 (dashboard)         → 1-2 jours
Phase 9 (docker + deploy)   → 1 jour
```

> **Les estimations initiales (2-3 jours pour les phases critiques) étaient trop optimistes.** Les phases 1, 4 et 6 sont les plus risquées — elles concentrent la quasi-totalité des bugs observés sur t-bot et tbot-scalp. Doubler le temps estimé est prudent.

**Note** : Phase 3 (stratégies) vient après l'exécution et le risk, pas avant. Raison : il faut pouvoir observer les features microstructure en live (dry-run) avant de décider quelles stratégies écrire. Les premières semaines seront de l'observation pure (recording + monitoring des features) avant de coder la moindre stratégie.

**Critères pour passer de l'observation aux stratégies** (ne pas skiper) :
- Au moins **2 semaines** de données L2 enregistrées sur tous les coins actifs
- Distribution de spread, imbalance et toxicity visualisées et comprises
- Fill rate ALO mesuré sur des ordres tests (dry-run) : si < 30% → reconsidérer l'approche
- Adverse selection rate mesuré : si > 60% des fills suivis d'un mouvement défavorable → les features microstructure ne donnent pas d'edge sur ce coin
