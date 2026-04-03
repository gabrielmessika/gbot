# Analysis Log — gbot dry-run sessions

> Journal d'analyse des sessions dry-run. Objectif : tracer ce qui a été testé, ce qui a marché ou pas, et les lecons tirées pour ne pas reproduire les mêmes erreurs.

---

## Session 1 — V1.0 (2026-04-01, 2h46, 28 trades)

### Config
- Signal : OFI + aggression persistence
- SL : 15 bps floor fixe, TP : 30 bps (RR=2.0)
- max_hold : 300s (5 min)
- 12 coins, pas de filtre trending

### Résultats
| Métrique | Valeur |
|----------|--------|
| Win Rate | 39% |
| P&L | -$90 |
| P&L/trade | -$3.23 |
| TP hits | 0 |
| Concentration ETH | 57% |

### Analyse
- **0 TP hit** en 2h46 — le TP à 30 bps est inatteignable en 5 min
- Les winners viennent du max_hold timeout (prix a bougé en faveur par chance)
- 11/12 premiers trades touchent SL en quelques secondes → SL=15bps est dans le bruit
- Direction score moyen 0.549, threshold 0.50 → signaux à peine au-dessus du seuil (bruit)
- Bug P0 : WS reconnect → DoNotTrade permanent pendant 16h

### Lecons
- **TP doit être atteignable dans la durée du hold** — 30bps en 5min = irréaliste
- **SL doit être > bruit du marché** — 15bps touché en secondes = trop serré
- **Le threshold trop bas = entrées sur du bruit**

---

## Session 2 — V1.1 (2026-04-02, 2h, 5 trades)

### Config
- Même signal (OFI), SL=8bps, TP=12bps (RR=1.5), max_hold=600s
- Fix WS reconnect, cooldown 120s

### Résultats
| Métrique | Valeur |
|----------|--------|
| Win Rate | 0% |
| P&L | -$15 |
| Trades | 5 |
| Concentration ETH | 77% |

### Analyse
- Seulement 5 trades en 2h — trop peu pour conclure
- ETH monopolise 77% du flow (10x plus de BookUpdates que les autres)
- OFI mesuré empiriquement : **corr=+0.058 avec ret30s, WR directionnel=2.3%** → c'est du bruit pur
- `aggression_persistence` colinéaire à OFI (corr=0.999 entre eux)
- `price_return_5s` : **corr=+0.354, WR=68% en trending** → signal 6x plus fort

### Lecons
- **OFI ne fonctionne pas sur Hyperliquid** — prouvé empiriquement, ne plus l'utiliser comme signal primaire
- **Le momentum prix (pr5s) est le seul signal avec un edge mesurable**
- **ETH monopolise à cause du différentiel de fréquence BookUpdate** — besoin d'un quota par coin

---

## Session 3 — V2.0 (2026-04-02, 4.4h, 172 trades)

### Config
- **Nouveau signal** : price momentum (pr5s/pr10s) remplace OFI
- SL=12bps, TP=18bps (RR=1.5), max_hold=45s
- Pullback timeouts indépendants (20s move + 20s retrace)
- Quota 6 signaux/coin/10min
- RangingMarket ajouté (trending_min_bps=3.0)
- Threshold 0.52

### Résultats
| Métrique | Valeur |
|----------|--------|
| Win Rate | **46.5%** |
| P&L | -$82 |
| P&L/trade | -$0.48 |
| Profit Factor | 0.80 |
| TP hits | 11 (6%) |
| SL hits | 41 (24%) |
| max_hold | 120 (70%) |
| ETH concentration | **18%** |
| Pullback completion | **36%** |

### Analyse temporelle
- **Q1 (36 premières minutes, marché trending)** : 43 trades, 10 TP, WR=60%, P&L=+$75
- **Q2-Q4 (marché flat)** : 129 trades, 1 TP, WR=42%, P&L=-$157
- Les 11 TP sont TOUS dans les 36 premières minutes — puis plus rien
- **RangingMarket : 0 déclenchements** → bug de placement dans classify() (après QuietTight)

### Analyse SL/TP
- SL avg=11 bps, TP avg=18 bps
- Mouvement moyen en 46s = **4.6 bps** → TP à 18 bps inatteignable pour la plupart des trades
- 70% des trades finissent en max_hold (ni TP ni SL atteints) avec P&L≈$0
- Les max_hold sont du bruit aléatoire — 49% WR, P&L total ≈ $0

### Lecons
- **Le signal momentum FONCTIONNE en marché trending** (Q1 : WR=60%, +$75)
- **Le bot ne doit PAS trader en marché flat** — le filtre RangingMarket est indispensable mais était bugé
- **SL/TP doivent matcher l'envelope de mouvement réaliste** — 18 bps TP pour 4.6 bps de mouvement moyen = irréaliste
- **La diversification coins fonctionne** — ETH de 77% à 18%, les 10 coins contribuent
- **Le pullback completion 36% est un progrès** (était 10%) mais peut encore s'améliorer

---

## Session 4 — V2.1 (2026-04-02 soir → 2026-04-03, 10h, 82 trades)

### Config (changements vs V2.0)
- Fix RangingMarket : déplacé AVANT QuietTight dans classify()
- **SL=5bps**, TP=10bps (**RR=2.0**)
- Threshold 0.52 → 0.60
- Trailing tiers abaissés (tier1=50%/30%, tier2=70%/50%)
- BE trigger 50% → 40%

### Résultats
| Métrique | V2.0 | **V2.1** | Sens |
|----------|------|----------|------|
| Win Rate | 46.5% | **13.4%** | ↓↓↓ |
| P&L/trade | -$0.48 | **-$3.46** | ↓↓↓ |
| Profit Factor | 0.80 | **0.10** | ↓↓↓ |
| SL hit % | 24% | **51%** | ↓↓ |
| TP hit % | 6% | 5% | → |
| max_hold % | 70% | 44% | ↑ |

### Analyse
- **SL=5bps est catastrophique** — mouvement moyen 2.9bps, SL à 1.7σ du bruit → 51% de SL hit par fluctuation aléatoire
- **RR=2.0 nécessite WR≥67%** — avec WR=13% c'est structurellement impossible
- **Fees round-trip (6bps) > SL distance (5bps)** → chaque BE "save" COÛTE de l'argent (-$1.80 par BE trade)
- Les 11 BE SL perdent -$20 au total au lieu de protéger
- RangingMarket fonctionne au snapshot (10/12 coins) mais 82 trades quand même (seuil 3bps trop bas)
- Biais Long 82% dans marché flat → Long WR=9%, Short WR=29%
- 0 TP dans aucun quartile temporel — uniformément mauvais

### Lecons CRITIQUES
- **JAMAIS mettre SL < 2× round-trip fees** — en dessous, le trade est structurellement perdant même si la direction est correcte. Fees maker+taker = 6bps minimum. SL ≥ 8bps minimum.
- **RR=2.0 est trop exigeant** pour du microstructure scalp — le WR nécessaire (67%) n'est pas atteignable. RR=1.5 (WR breakeven=40%) est le max réaliste.
- **Le trending_min_bps=3.0 est insuffisant** — avec 6bps de fees, un marché qui bouge <5bps n'a aucun edge exploitable.
- **Baisser le SL ne résout pas le problème du TP inatteignable** — ça empire les choses en ajoutant du bruit SL.

---

## Session 5 — V2.2 (2026-04-03, en cours)

### Config (changements vs V2.1)
- SL : 5 → **8 bps** (règle : ≥ 2× fees)
- RR : 2.0 → **1.5** (TP ~12bps, breakeven WR=40%)
- trending_min_bps : 3.0 → **5.0**
- min_direction_confirmations : 3 → **5**
- BE trigger : 40% → **50%** (50% de 12bps = 6bps = couvre les fees)
- Trailing : tier1=60%/30%, tier2=80%/50%

### Evolutions structurelles ajoutées (V2.3)
- **TP sortie maker (ALO limit)** au lieu de trigger order → round-trip fees 6 → 3 bps
- **Signal inverse exit** : force exit IOC si pr5s fortement opposé à la position
- **Stale quote management** : cancel TP ALO si toxicité/régime hostile
- **Smart max hold** : sortie anticipée à 70% du max_hold si en perte
- **Fee accounting corrigé** : dry-run distingue maker/taker, SL = taker fee

### Résultats
*En attente — le bot vient de se stabiliser (10 coins, WS stable, marché en RangingMarket).*

---

## Bugs infrastructure rencontrés

### WS reconnect loop (2026-04-03)
- **Cause 1** : 31 coins (62 WS subs) → Hyperliquid coupe la connexion
- **Cause 2** : backoff jamais appliqué — `connect_and_listen()` retournait `Ok` même après 1s → delay reset à chaque cycle → spam reconnect (420 reconnects en 8min → ban IP)
- **Cause 3** : burst de subscriptions — 30 messages envoyés en 0ms → reset immédiat
- **Fix** : pacing 250ms entre subs, backoff reset seulement si connexion >30s, initial_delay 5s, max 60s
- **Limite confirmée** : 10 coins (20 subs + allMids + orderUpdates) = stable. 15 coins (30 subs) = reset systématique même avec pacing.

### Fee accounting (2026-04-02)
- **TP exits = trigger orders = taker (4.5bps)**, pas maker comme prévu dans le plan original
- Le dry-run ne comptait aucun fee de sortie → P&L trop optimiste
- Le live comptait maker (1.5bps) pour TOUS les exits y compris SL → fees sous-estimées
- Fix V2.3 : TP = ALO limit (maker), SL = trigger (taker), dry-run calcule les fees correctement

---

## Règles empiriques établies

### Signal
| Règle | Source |
|-------|--------|
| OFI est du bruit sur Hyperliquid (corr=0.058) | Session 2 analyse |
| price_return_5s est le signal dominant (corr=0.354) | Session 2 analyse |
| Momentum WR=68% en trending, WR=0% en ranging | Session 2 simulation |
| L'autocorrélation des returns tombe à 0 à 60s | Session 2 analyse |
| Biais Long systématique en marché flat → le signal capte du bruit | Session 4 |

### SL/TP
| Règle | Source |
|-------|--------|
| **SL ≥ 2× round-trip fees** (6bps si taker exit, 3bps si maker exit) | Session 4 — V2.1 SL=5bps → 51% SL hit |
| SL trop loin (12-15bps) → max_hold timeout (70%) | Sessions 1, 3 |
| SL trop serré (5bps) → bruit SL (51%) | Session 4 |
| **SL sweet spot : 8-10 bps** | Convergence sessions 3+4 |
| TP doit être atteignable dans max_hold_s | Sessions 1, 3 |
| Mouvement moyen en 46s = 3-5 bps | Sessions 3, 4 |
| RR=2.0 nécessite WR≥67% (irréaliste) | Session 4 |
| **RR=1.5 (WR breakeven=40%) est le max réaliste** | Convergence |

### Fees
| Règle | Source |
|-------|--------|
| Round-trip maker/taker = 6 bps | Documentation Hyperliquid |
| Round-trip maker/maker = 3 bps | V2.3 TP ALO |
| **Les fees mangent 25-50% du TP** — chaque bps compte | Calcul |
| BE SL coûte de l'argent si SL < fees | Session 4 — 11 BE = -$20 |

### Filtre trending
| Règle | Source |
|-------|--------|
| trending_min_bps=3.0 laisse passer trop de flat | Session 4 |
| **trending_min_bps=5.0** minimum (> fees) | Session 4 analyse |
| En ranging, le signal momentum a WR=0% | Session 2 simulation |

### Infrastructure
| Règle | Source |
|-------|--------|
| Max ~10 coins par connexion WS (20 subs) | 2026-04-03 tests |
| Espacer les subscriptions WS (250ms/coin) | 2026-04-03 fix |
| Le backoff doit vérifier la durée de connexion, pas juste Ok/Err | 2026-04-03 bug |
| Ne jamais spammer les reconnects (ban IP en minutes) | 2026-04-03 ban |

---

## Paramètres optimaux actuels (V2.2 + V2.3)

```toml
# Signal
w_pr5s = 0.40
w_pr10s = 0.20
direction_threshold = ±0.60
min_direction_confirmations = 5
trending_min_bps = 5.0

# SL/TP
sl_min_bps = 8.0
sl_max_bps = 30.0
sl_vol_multiplier = 3.0
target_rr = 1.5        # TP ~12bps
# TP = ALO limit (maker 1.5bps), SL = trigger (taker 4.5bps)

# Timing
max_hold_s = 45
pullback_wait_move_s = 20
pullback_wait_retrace_s = 20
pullback_min_move_bps = 1.5

# Risk
max_signals_per_coin_10min = 6
breakeven_trigger_pct = 50.0  # 50% de TP = 6bps = couvre les fees
```
