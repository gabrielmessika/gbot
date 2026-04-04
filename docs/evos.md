# Evolutions gbot — Diagnostic & Plan

> Mis a jour 2026-04-04 apres backtest complet (4 jours, 5457 trades).
> **Conclusion : le signal microstructure n'a pas d'alpha a horizon 45s sur Hyperliquid.**
> Pivot vers Passivbot (grid/MM) + recherche d'un vrai signal predictif.

---

## Contraintes Hyperliquid (rappel)

| Contrainte | Valeur | Impact |
|-----------|--------|--------|
| Position par coin | **1 seule (net)** — long OU short | Bloque market-making, grid hedge |
| Ordres pending par coin | **Multiples autorises** | Grid possible via ordres multiples |
| Fees maker | **-1.5 bps** (rebate) | Round-trip maker/maker = 3 bps |
| Fees taker | **+4.5 bps** | Round-trip maker/taker = 6 bps |
| Min notional | **$11 USD** | Pas de micro-trades |
| Max coins WS | ~10-15 coins stables | Pas de scanning 50 coins |

---

## Diagnostic : pourquoi gbot n'est pas rentable

### Le signal n'a pas d'alpha

Donnees : 83 trades Session 5A (overnight 2-3 avril), features au moment de l'entree.

| Metrique | Winners (n=12) | Losers (n=71) | Ecart |
|----------|---------------|--------------|-------|
| |dir_score| | 0.674 | 0.672 | **0.002** — zero |
| queue_score | 0.776 | 0.805 | ~0 |
| spread_bps | 0.223 | 0.331 | faible |
| toxicity | 0.132 | 0.116 | ~0 |
| vol_ratio | 1.077 | 0.936 | ~0 |

**Le direction score ne discrimine pas les winners des losers.** Les 6 features (pr5s, pr10s, micro_price, vamp, depth_imb, toxicity) sont correlees au mouvement passe, pas au mouvement futur.

### Les fees mangent tout

| Date | Trades | WR | P&L | Fee drag | MFE moyen |
|------|--------|-----|------|----------|-----------|
| 01/04 | 454 | 27.8% | -$108 | 115% | 2.78 bps |
| 02/04 | 1918 | 21.1% | -$868 | 134% | — |
| 03/04 | 2967 | 4.5% | -$3810 | 369% | — |
| 04/04 | 118 | 17.8% | -$113 | 213% | 1.52 bps |

Fee drag > 100% = les fees depassent le profit brut. Le MFE moyen (1.5-2.8 bps) est inferieur aux fees round-trip (3 bps).

### Le risk management ne protegeait pas

Session 4 avril (dry-run) : **37 trades consecutifs sur ETH Short, WR 0%, -$286.**
- `max_signals_per_coin_10min=6` + `cooldown=120s` = 16 trades/h sur 1 coin
- Aucun mecanisme de detection de serie perdante

Session overnight : 66 Longs / 17 Shorts pendant que BTC baissait de 68064 a 66861.
- Le bot etait biais Long dans un marche baissier

### Ce qu'on a essaye (tout echoue)

| Approche | Resultat | Pourquoi ca echoue |
|----------|----------|-------------------|
| Mean-reversion flat (EVO-1/2/3) | WR 10.5%, -$6785 | MFE < fees |
| False breakout fade (EVO-4) | WR 3%, -$582 | TP < fees |
| Concordance momentum (EVO-12) | WR 22%, -$108 a -$3810 | Signal sans alpha, filtre insuffisant |
| SL 5/8/12/15 bps | Tous negatifs | Le SL n'est pas le probleme |
| Confirmations 3/5 | 0 trades (5) ou negatifs (3) | Filtrer du bruit = moins de bruit, pas de signal |
| RR 1.5/2.0 | Tous negatifs | TP inatteignable |

### Le vrai probleme

```
Le probleme de gbot n'est PAS :
  x les filtres d'entree (resserres 4 fois, WR ne bouge pas)
  x les sorties (pas de winners a mieux gerer)
  x la detection de regime (flat detecte, trending pas rentable non plus)
  x le SL/TP (teste 5/8/12/15 bps, meme resultat)

Le probleme de gbot EST :
  > le signal microstructure n'a pas d'alpha a horizon 45s
  > les features sont correlees au mouvement PASSE, pas FUTUR
  > les fees (3 bps) > amplitude exploitable (1.5-2.8 bps MFE)
  > aucun filtre ne peut sauver un signal sans edge
```

---

## Actifs : ce qui reste valable

### Infrastructure

- WS reconnect + backoff corriges
- Risk state persistence (peak equity, daily reset)
- Backtester avec sizing reel, MAE/MFE, multi-date
- TP ALO maker (fees 3 bps au lieu de 6)
- Recorder L2 multi-level (10 levels bid/ask) — precieux pour la recherche
- Signal inverse exit + smart max hold

### Risk management (nouvellement implemente)

- `max_signals_per_coin_10min = 2` (etait 6 — ETH monopolisait)
- Streak breaker : 3 SL consecutifs -> coin bloque 30 min
- Filtre concordance momentum : sign(pr5s) == sign(pr10s)

---

## Plan d'action

### PRIORITE 1 — Passivbot (grid trading externe)

**Pourquoi** : Passivbot ne predit pas la direction. Il capture le spread + rebates maker.
Sur un marche flat, c'est le bon modele economique : profiter du va-et-vient au lieu de le predire.

**Etapes** :
1. Installer passivbot, configurer Hyperliquid
2. Paper trading 3-5 coins (coins differents de gbot pour eviter conflits)
3. Lancer l'optimiseur sur donnees historiques
4. 48-72h paper trading, mesurer P&L / DD / fill rate

**Validation** : P&L positif en paper trading 72h. Sharpe > 0.5 annualise.

**Contraintes** :
- Coins differents de gbot (pas de conflit position)
- Subaccount separe si possible
- Alertes si DD > seuil

### PRIORITE 2 — Recherche de signal (avant tout nouveau code)

**Pourquoi** : avant de recoder gbot, il faut trouver un predicteur qui a un vrai alpha.
Le notebook d'analyse doit repondre a : "quelle feature predit le mouvement futur ?"

**Analyses a faire** :
1. **Correlation features x return futur** — pour chaque feature, mesurer la correlation
   avec le mid price a +5s, +10s, +30s, +60s, +300s. Trouver l'horizon ou l'alpha existe.
2. **Delta OFI** — l'OFI niveau (corr=0.058) est faible, mais le *changement* d'OFI
   (acceleration du flow) predit peut-etre mieux.
3. **Large trade detection** — les sweeps (trades > 5x taille mediane) comme signal
   de momentum institutionnel.
4. **Cross-coin lead-lag** — BTC bouge -> SOL/ETH suivent avec un lag de quelques secondes.
   Mesurer les correlations croisees decalees.
5. **Horizon optimal** — le momentum a 30-60s a peut-etre un pouvoir predictif a 5-15 min
   que le bruit noie a 45s. Tester max_hold = 300-900s en backtest.

**Livrable** : un rapport avec les correlations et un candidat signal avec edge mesurable.

### PRIORITE 3 — Pivoter gbot si signal trouve

Si l'analyse trouve un predicteur viable (corr > 0.15, WR > breakeven apres fees) :
- Adapter l'horizon de trading (max_hold, SL/TP)
- Implanter le nouveau signal dans compute_direction_score()
- Backtester sur les 4 jours
- Dry-run 48h

### BACKLOG

| EVO | Statut | Note |
|-----|--------|------|
| EVO-8 HMM Regime | Suspend | Inutile sans alpha dans le signal |
| EVO-10 Market Making A-S | Potentiel | Via Hummingbot, pas gbot. Apres Passivbot |
| EVO-13 Trailing dynamique | Suspend | Pas de winners a mieux gerer |

---

## Historique complet des EVOs

### ABANDONNEES — flat market MR
- EVO-1 Mean-Reversion : WR 10.5%, -$6785, MFE < fees
- EVO-2 Squeeze Detection : filtre inutile (strat sous-jacente non viable)
- EVO-3 Vol Spike MR : MFE post-spike insuffisant
- EVO-4 False Breakout Fade : WR 3%, -$582, TP < fees
- EVO-5 Bandes de vol : variante MR, meme probleme structurel
- EVO-6 Z-Score OU : variante MR, meme probleme structurel
- EVO-9 Scalping ultra-serre : dependait de WR MR > 75%

### IMPLEMENTEES MAIS INSUFFISANTES
- EVO-12 Concordance momentum : WR 22% (vs 14% avant), toujours < breakeven 40%
- Risk: quota coin 6->2, streak breaker 3 SL, concordance pr5s/pr10s
