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

### Résultats (après 14h de run, 02:00→16:07 UTC)

#### Phase A — Overnight avec min_confirmations=3 (00:18→07:15, 7h)
| Métrique | Valeur |
|----------|--------|
| Trades | 46 |
| Win Rate | **37.0%** |
| P&L | **-$14.93** |
| P&L/trade | -$0.32 |
| TP hits | 3 (7%) |
| SL hits | 26 (57%) |
| max_hold | 17 (37%) |
| Long/Short | 33L / 13S |
| Long WR | 30% |
| Short WR | **54%** |

Meilleure session jusque-là : P&L/trade quasi-neutre malgré SL=5bps/TP=10bps (pire config V2.1).
Le biais Long persiste (72% Long, WR=30% vs Short WR=54%).

#### Phase B — Journée avec min_confirmations=5 (09:45→16:07, 6h+)
| Métrique | Valeur |
|----------|--------|
| Trades | **0** |
| Signaux générés | 323 |
| Signaux armés (pullback) | 140 setups |
| Micro-moves confirmés | 20 (14%) |
| Pullbacks complétés | **0** |
| Pullback expired (move timeout) | 140 |
| Direction confirmations 5/5 | **0** |

Zéro trade en 6+ heures malgré 323 signaux valides (passent régime + direction score > 0.60).

### Analyse du blocage (double filtre)

**Filtre 1 : min_direction_confirmations = 5 (vs 3 overnight)**
- 1010 incréments de confirmation loggés, 0 atteignent 5/5
- Distribution : 1/5=186, 2/5=174, 3/5=167, 4/5=157 → les signaux s'inversent avant d'atteindre 5
- Avec threshold=3, la session overnight a tradé 46 fois avec le même marché

**Filtre 2 : Pullback completion rate effondré en journée**
- Overnight (confirmations=3) : 93/154 micro-moves (60%), 46 trades
- Journée (confirmations=5) : 20/140 micro-moves (14%), 0 trades
- Le min_move de 1.5bps est atteignable en micro-structure active (nuit), mais pas en marché quiet (jour)

### Analyse du marché (RangingMarket n'est pas le problème)

Le régime RangingMarket a dominé **les deux phases** :
- 07:15 (fin trading) : 10/12 coins en RangingMarket, 1 ActiveHealthy, 1 QuietTight
- 16:07 (0 trades) : 8/10 coins en RangingMarket, 2 QuietTight

Distribution de `|price_return_30s|` pour BTC aujourd'hui :
- Mean : 0.25 bps (largement < 5.0 threshold)
- P90 : 0.00 bps
- **\>5 bps : 0.8% des ticks** seulement
- \>3 bps : 1.9%, >2 bps : 3.1%

Même le 02/04 (qui avait 183 trades V2.0), le profil était similaire : BTC mean=0.20bps, >5bps=1.3%.
Les signaux qui passent le filtre trending_min_bps=5.0 existent (~1% des ticks → 323 signaux/jour) mais le **combo confirmation×5 + pullback move timeout** les bloque tous.

### Est-ce que RangingMarket est trop strict ?

**Non, le filtre trending en lui-même est justifié** — les sessions V2.0 Q2-Q4 et V2.1 prouvent que trader en marché flat = WR 14-42%, P&L toujours négatif. La corrélation momentum est 0 en ranging (Session 2).

**Le problème est le min_direction_confirmations=5** qui, en combinaison avec le filtre trending, crée un dead zone : les rares fenêtres >5bps durent trop peu pour que 5 confirmations consécutives se cumulent avant que le marché retourne en ranging.

### Recommandation

1. **Revenir à min_confirmations=3** — prouvé overnight (46 trades, P&L quasi-neutre). Le passage à 5 a supprimé 100% de l'activité sans bénéfice mesurable.
2. **Garder trending_min_bps=5.0** — empiriquement validé, le signal n'a pas d'edge <5bps.
3. **Observer la Phase A** : avec SL=8bps (au lieu de 5bps overnight) + TP ALO maker, les résultats pourraient s'améliorer significativement. La Phase A avait SL=5bps (config V2.1 résiduelle) mais WR=37%, proche du breakeven de 40%.

---

## Session 6 — V2.2b (2026-04-04, 8h, 0 trades)

### Config (changements vs V2.2)
- min_direction_confirmations : 5 → **3** (fix Session 5B)
- Reste identique : SL=8bps, RR=1.5, trending_min_bps=5.0, pullback_min_move_bps=1.5

### Résultats (02:09→09:45 UTC, 8h de run)
| Métrique | Valeur |
|----------|--------|
| Trades | **0** |
| Signaux générés | 46 |
| Confirmations 3/3 atteintes | **46** (100% — fix fonctionne) |
| Pullback setups | 45 |
| Micro-move confirmés | 6 (13%) |
| Retrace timeout | 6 (100% des micro-moves) |
| Move timeout | 38 (84%) |
| Pullback complétés | **0** |

### Analyse

**Le fix confirm=3 fonctionne** — 46/46 signaux passent la confirmation (vs 0/1010 en Session 5B avec confirm=5). Le pipeline est débloqué jusqu'au pullback.

**Le blocage est maintenant purement le pullback + marché ultra-flat** :
- 84% des setups expirent en WaitingMove (prix ne bouge pas de 1.5bps en 20s)
- Les 6 qui passent (13%) expirent en WaitingPullback (mouvement unidirectionnel, pas de retrace)

**Marché du 04/04 = le plus flat depuis le lancement :**
| Coin | |pr30s| mean | >5bps | Régime |
|------|-------------|-------|--------|
| BTC | 0.16 bps | 0.8% | RangingMarket |
| ETH | 0.18 bps | 0.6% | RangingMarket |
| SOL | 0.21 bps | 0.8% | RangingMarket |
| HYPE | 0.47 bps | 1.8% | RangingMarket |

9/10 coins en RangingMarket, 1 en ActiveToxic. Aucun coin tradable.

**Biais Long confirmé dans les signaux** : 29 Long / 17 Short (63% Long), cohérent avec les sessions précédentes.

**HYPE monopolise** : 26/46 signaux (57%). L'imbalance HYPE est plus volatile → le signal momentum s'y déclenche plus souvent, mais sans mouvement réel.

### Lecons
- **Le pullback est le dernier goulot d'étranglement** — une fois la confirmation débloquée, c'est le micro-move rate qui détermine si le bot trade
- **Marché ultra-flat = pas de trades = comportement correct** — trader dans ce marché = perte garantie (ref: Session 4 WR=14%)
- **La config V2.2b est prête** — il faut juste un marché coopératif pour valider SL=8bps + TP ALO maker + confirm=3

### Données complémentaires Session 6 (04/04 09:53 UTC snapshot)

**Pipeline complet confirmé sur 8h+ de données :**
```
Signaux générés:           46
Confirmations 3/3:         46 (100% — fix confirm=3 fonctionne)
Pullback setups:           45
Micro-move confirmés:       6 (13%)
Retrace timeout:            6 (100% des micro-moves)
Move timeout:              38 (84% des setups)
Pullback complétés:         0
Trades:                     0
```

**Direction confirmations :** 48× 1/3, 47× 2/3 en plus des 46 ayant atteint 3/3. Le pipeline direction est débloqué.

**Régimes au snapshot :** 9/10 RangingMarket, 1 ActiveToxic (ARB). 0 coins tradables (ni QuietTight ni ActiveHealthy).

**Biais Long dans les signaux :** 29 Long / 17 Short (63/37%). HYPE monopolise 57% des signaux (26/46).

**Bot status :** Equity $10,000 (simulated), 0 positions, 0 daily P&L, 10 WS reconnects, 0 kill switches, uptime 17h.

---

## Session intermédiaire — V1.1/V2.0 bridge (2026-04-02, 3h, 15 trades)

### Contexte
Journal `journal_2026-04-02_10-39-39.jsonl` — session intermédiaire entre V1.1 et V2.0, probablement config transitionnelle. Non documentée dans le log original.

### Résultats
| Métrique | Valeur |
|----------|--------|
| Trades | 15 |
| Win Rate | 33.3% |
| P&L | -$2.56 |
| P&L/trade | -$0.17 |
| Profit Factor | 0.92 |
| SL hits | 8 (53%) |
| TP hits | 4 (27%) — meilleur taux TP de toutes les sessions |
| max_hold | 1 (7%), regime exit: 2 (13%) |

### Analyse
- **4 TP hits en 15 trades (27%)** — nettement supérieur à toutes les autres sessions (max 6% S3)
- SL avg -$3.57, TP avg +$7.15 → RR effectif ≈ 2.0 quand le TP touche
- ETH monopolise 60% (9/15 trades) — quota pas encore actif
- PF 0.92 = presque breakeven malgré un WR de 33%

### Lecon
- Les 4 TP hits suggèrent un marché plus trending pendant cette session (13-16h UTC)
- Avec un meilleur WR (via réduction SL hits), cette config aurait pu être profitable

---

## Session fragment — V2.2 post-Session 4 (2026-04-03, 6min, 9 trades)

### Contexte
Journal `journal_2026-04-03_05-28-03.jsonl` — 9 trades en 6 minutes juste après la fin de Session 4, bot redémarré à 07:28. Config V2.2 (SL=8bps, confirm=3).

### Résultats
| Métrique | Valeur |
|----------|--------|
| Trades | 9 |
| Win Rate | 44.4% |
| P&L | -$24.01 |
| max_hold exits | 6 (67%), WR=67%, avg +$0.44 |
| SL hits | 3 (33%), avg -$8.89 |
| Directions | 100% Long |

### Analyse
- Les 3 SL hits pèsent lourd (-$8.89 chacun) vs les max_hold wins (+$0.44)
- **Asymétrie SL/gain très défavorable** : le SL à 8bps coûte ~$9 mais les trades gagnants via max_hold font <$3
- 100% Long — le biais Long persiste même avec confirm=3 et threshold=0.60
- Session trop courte (6min) pour conclure, mais le pattern "gros SL, petits gains" est cohérent avec Sessions 1-4

### Lecon
- Les trades max_hold positifs restent marginaux ($0.44 avg) — confirme que le profit vient des TP, pas du hasard max_hold
- L'asymétrie SL loss (-$9) vs max_hold gain (+$0.44) est le problème structurel : **il faut des TP hits pour compenser les SL**

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
| min_confirmations=5 bloque 100% des trades (0/1010 atteignent 5/5) | Session 5 |
| min_confirmations=3 permet 46 trades/7h avec WR=37%, P&L≈neutre | Session 5 Phase A |

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
| trending_min_bps=5.0 est correct (pas trop strict) — BTC >5bps ~1% des ticks, suffisant pour 300+ signaux/jour | Session 5 |
| Le blocage n'est PAS le trending filter mais le combo confirmations=5 + pullback move_timeout | Session 5 |
| Avec confirm=3, le pullback micro-move devient le goulot (84% move timeout en marché flat) | Session 6 |
| Marché ultra-flat (pr30s < 0.5bps mean) = 0 trades = comportement correct, pas un bug | Session 6 |
| Le pullback retrace est un second filtre : 100% des micro-moves expirent en retrace timeout en marché flat | Session 6 (04/04 data) |
| HYPE monopolise les signaux en flat (57% sur 04/04) — imbalance plus volatile mais sans edge | Session 6 |

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
min_direction_confirmations = 3   # 5 bloque 100% des trades — Session 5
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
