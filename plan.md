# gbot — Plan d'évolution MFDP V2

> **Statut** : **Phases 1, 2, 3 et 4.1 IMPLÉMENTÉES** (2026-04-02). Build propre. Prêt pour dry-run de validation.
> Ce plan documente les évolutions prioritaires issues de l'analyse empirique sur données L2 réelles (2026-04-01/02, 20h, 12 coins).
>
> **Règle** : toute évolution doit avoir un résultat attendu chiffré et être validée en dry-run
> sur ≥ 4h avant passage en live.

---

## Résultats empiriques — base factuelle de ce plan

### Ce qui a été mesuré sur les données réelles

| Feature | corr → ret30s | directional_acc | Verdict |
|---------|--------------|-----------------|---------|
| OFI_10s | +0.058 | **2.3%** | Bruit. Inutile. |
| TFI (notionnel) | +0.058 | 2.3% | Identique à OFI sur ce data. |
| depth_imbalance (L1) | +0.259 | 7.8% | Faible mais réel. |
| price_return_5s | **+0.354** | **40.5%*** | Signal dominant. |
| price_return_30s (passé) | +0.342 | 25.2% | Momentum durable. |

*40.5% = contexte baissier : le signal capture le trend, pas un edge générique.

**Autocorrélation des returns ETH (series 5s) :**

| Lag | Autocorr | Interprétation |
|-----|----------|----------------|
| 5s | +0.262 | Momentum fort |
| 10s | +0.155 | Momentum présent |
| 30s | +0.128 | Momentum résiduel |
| 60s | +0.014 | Disparaît |

→ Le marché est trending sur 5-30s, puis revient à la moyenne. Le `max_hold=300s` actuel est **20× trop long**.

**Simulation momentum (avec 0.5 bps spread, horizon 30s) :**

| Fenêtre | Seuil | N trades | WR | P&L moyen |
|---------|-------|----------|----|-----------|
| 5s | 1 bps | 63 | **68.3%** | **+7.3 bps** |
| 5s | 2 bps | 46 | 67.4% | +7.5 bps |
| 10s | 2 bps | 79 | 63.3% | +6.3 bps |
| 20s | 2 bps | 138 | 55.8% | +5.1 bps |
| Baseline always-long | — | — | — | **−0.25 bps** |

**Limite critique** : ces résultats sont sur un marché baissier continu. Le signal en régime ranging donne WR = 0%. Le filtre trending est **non-optionnel**.

---

## Problèmes actuels V1

### P1 — Signal principal OFI = bruit (cause racine)
`dir_score ≈ 0.4 × ofi_10s + bruit`. OFI et `aggression_persistence` mesurent la même chose (corrélation 0.999). Précision directionnelle : 2.3%. La stratégie est structurellement aléatoire.

### P2 — Horizon de hold 300s incompatible avec le momentum
L'autocorrélation des returns tombe à ~0 à 60s. Tenir une position 5 minutes revient à laisser le marché décider aléatoirement.

### P3 — Pas de filtre trending/ranging
En régime ranging (marché flat sur 5 min), le momentum 5s a une précision directionnelle de **0%**. Le bot trade dans tous les contextes.

### P4 — Pullback timeout 30s partagé entre 2 phases
`WaitingMove` (attendre micro-move ≥ 3 bps) + `WaitingPullback` (retrace 35% + OFI confirm) partagent le même budget de 30s. En pratique, `WaitingMove` consomme ~20s → `WaitingPullback` n'a pas le temps de compléter → abandon → rearm → boucle infinie de setups avortés (45/50 signaux ETH en session 2).

### P5 — ETH monopolise le flow
ETH reçoit ~10× plus de `BookUpdate` que XRP/HYPE → passe les 3 confirmations consécutives plus facilement. Pas de limitation par coin.

### P6 — Multi-level OBI non testé (données insuffisantes)
Les données L2 ne contenaient que best_bid/best_ask jusqu'au 2026-04-02. Les niveaux L1-L10 sont maintenant enregistrés mais il n'y a pas encore assez de données pour valider.

---

## Plan d'évolution

### PHASE 1 — Signal ✅ FAIT
**Objectif : remplacer OFI par price momentum comme signal primaire.**

#### 1.1. ✅ Ajouter `price_return_Ns` dans FlowFeatures
**Fichier** : `src/features/flow_features.rs`

Implémenté : champs `price_return_5s`, `price_return_10s`, `price_return_30s` + fonction `compute_price_return()`.
Calcul : `(last_price - first_price) / first_price × 10_000` bps depuis le tape.

**Résultat attendu** : corrélation signal → ret30s passe de 0.058 (OFI) à ~0.25-0.35 (pr5s). *À mesurer en dry-run.*

#### 1.2. ✅ Refactorer `compute_direction_score` dans MfdpStrategy
**Fichier** : `src/strategy/mfdp.rs`

Formule implémentée :
```
dir_score = w_pr5s × sign(pr5s) × min(|pr5s|/5, 1)     // momentum court terme
          + w_pr10s × sign(pr10s) × min(|pr10s|/10, 1)  // confirmation
          + w_micro_price × micro_norm                    // micro-price
          + w_vamp × vamp_norm                            // ancillaire
          + w_depth_imb × imbalance_weighted              // profondeur multi-niveau signée
          - w_toxicity × toxicity                         // filtre adversarial
```

`w_ofi` et `w_aggression` supprimés (colinéaires, corr=0.999 entre eux).

Config `default.toml` appliquée :
```toml
w_pr5s = 0.40   w_pr10s = 0.20   w_micro_price = 0.15
w_vamp = 0.15   w_depth_imb = 0.15   w_toxicity = 0.10
```

**Résultat attendu** : dir_score corrélé ~0.30-0.40 avec ret30s vs 0.058 actuellement. *À mesurer en dry-run.*

#### 1.3. ✅ Ajouter filtre trending dans le regime engine
**Fichier** : `src/regime/engine.rs`

Implémenté : `Regime::RangingMarket` — `allows_entry() = false`.
Condition : `|price_return_30s| < trending_min_bps` (défaut : 3.0 bps).
Placement dans la cascade : après LowSignal, avant le fallback ActiveHealthy.

**Résultat attendu** : élimination des trades en régime ranging (WR=0% mesuré). *À mesurer en dry-run.*

---

### PHASE 2 — Timing et horizon ✅ FAIT
**Objectif : aligner l'horizon de hold avec la durée réelle du signal.**

#### 2.1. ✅ Réduire `max_hold_s` de 300s à 45s
**Fichier** : `config/default.toml`

Implémenté : `max_hold_s = 45`. L'autocorrélation des returns tombe à ~0 à 60s.

**Résultat attendu** : les trades "ni TP ni SL" disparaissent. *À mesurer en dry-run.*

#### 2.2. ✅ Séparer les timeouts des phases pullback
**Fichiers** : `src/strategy/pullback.rs`, `src/config/settings.rs`, `config/default.toml`

Implémenté : `max_wait_ms` → `wait_move_ms` + `wait_retrace_ms` indépendants.
`expires_at` resetté à `now_ms + wait_retrace_ms` à la transition `WaitingMove → WaitingPullback`.
Config : `pullback_wait_move_s = 20`, `pullback_wait_retrace_s = 20`.

**Résultat attendu** : taux de complétion pullback 10% → 40-60%. *À mesurer en dry-run.*

#### 2.3. ✅ Réduire `pullback_min_move_bps` de 3.0 à 1.5
**Fichier** : `config/default.toml`

Implémenté : `pullback_min_move_bps = 1.5`.
1.5 bps ≈ 3.7σ de la vol 30s : signal directionnel clair mais accessible (3.0 bps = 7.5σ = rare).

**Résultat attendu** : `WaitingMove` complète en 3-8s au lieu de 15-25s. *À mesurer en dry-run.*

---

### PHASE 3 — Diversification ✅ PARTIELLEMENT FAIT
**Objectif : casser la monopolisation ETH.**

#### 3.1. ✅ Limiter les signaux par coin par fenêtre de temps
**Fichiers** : `src/main.rs`, `src/config/settings.rs`, `config/default.toml`

Implémenté : `coin_signal_timestamps: HashMap<String, VecDeque<i64>>`, fenêtre glissante 10min.
Avant chaque émission : purge des timestamps > 10min, vérification quota.
Config : `max_signals_per_coin_10min = 6`.

**Résultat attendu** : ETH passe de 77% du flow à <50%. *À mesurer en dry-run.*

#### 3.2. ⏳ Cooldown adaptatif après SL_HIT vs max_hold
**Fichier** : `src/main.rs`

Non encore implémenté. Cooldown actuel : fixe.
Plan : après SL_HIT → `2 × hold_duration`. Après max_hold (ni TP ni SL) → 30s seulement.

---

### PHASE 4 — Analyse multi-level OBI
**Objectif : valider si OBI L1-L10 apporte un edge mesurable.**

#### 4.1. ✅ Script d'analyse offline
**Fichier** : `research/scripts/analyze_obi_levels.py` — **créé**

Calcule `corr(OBI_Ln, ret_30s)` et `directional_acc` pour L1 à L10.
Critère de décision intégré : `|corr_LN| >= 2 × |corr_L1|` pour justifier l'implémentation.
Usage : `python analyze_obi_levels.py --data-dir ./data/l2 --coin ETH --date 2026-04-05`

#### 4.2. ⏳ Lancer l'analyse (attente de données suffisantes)

Données `bid_levels[10]` / `ask_levels[10]` enregistrées depuis le **2026-04-02** (`recorder.rs`).
**Données nécessaires** : ≥ 3 jours avec régimes variés. Date possible : ~**2026-04-05**.

```bash
# Vérifier que les niveaux sont présents dans les données
head -1 data/l2/ETH/2026-04-03.jsonl | python3 -c "
import sys, json
d = json.loads(sys.stdin.read())
print(f'bid_levels: {len(d.get(\"bid_levels\",[]))} niveaux')
print(f'ask_levels: {len(d.get(\"ask_levels\",[]))} niveaux')
"
```

#### 4.3. ⏳ Intégration (conditionnel)
Si l'analyse montre `|corr_L5+| >= 2 × |corr_L1|` → intégrer `OBI_weighted` dans `dir_score`.
Sinon → `depth_imbalance` actuel (imbalance_weighted, L1-L5 approximé) suffit.

**Résultat attendu selon la littérature** : OBI_L5-L10 améliore la corrélation de 15-25% à horizon <10s.
Sur Hyperliquid (peu de spoofing, book transparent), l'amélioration pourrait être moindre.

---

### PHASE 5 — Hyperliquid-specific alpha (moyen terme)
**Objectif : exploiter les données exclusives à Hyperliquid non disponibles sur CEX.**

Ces signaux sont uniques à Hyperliquid (book on-chain transparent) et non implémentés dans aucun repo public. À développer une fois que les phases 1-3 sont stables.

#### 5.1. Liquidation map
Via `clearinghouseState` REST : calculer la distance des positions ouvertes agrégées aux prix de liquidation. Si $X millions se liquident dans les Y bps → pression directionnelle prévisible.

**Critères d'intégration** : implémenter en Python d'abord pour valider le signal sur historique, puis porter en Rust si corrélation > 0.15 mesurée.

#### 5.2. Funding rate comme filtre macro
Règle simple : funding > +0.08%/8h → biais short uniquement (longs surreprésentés). Funding < -0.08%/8h → biais long uniquement. Aucune entrée dans la direction du funding extrême.

**Critères d'intégration** : tester en dry-run sur 1 semaine. Si filtrage des mauvais trades >20% → intégrer.

---

## Résultats attendus globaux

### Après Phases 1-3 (baseline actuelle → V2)

| Métrique | Session 1 (V1 old) | Session 2 (V1 new) | V2 cible |
|----------|-------------------|-------------------|----------|
| Win Rate | 39% | 0% | **55-65%** |
| P&L/trade | −$3.23 | −$3.12 | **+$2 à +$5** |
| Trades/heure | 3.9 | 2.3 | 2-5 |
| Concentration ETH | 57% | 77% | <35% |
| Trades en régime ranging | non mesuré | ~100% | **<20%** |

**Hypothèses sous-jacentes :**
- Signal momentum pr5s → WR 55-65% en marché trending (mesuré empiriquement : 68% sur session baissière continue)
- Filtre trending réduit le volume de trades de 40-60% mais élimine les trades WR~0%
- Horizon 45s aligne le hold avec la durée du signal (autocorr > 0.10)
- Diversification coins réduit la drawdown par corrélation trop forte

**Avertissement** : les 68% WR simulés sont sur un seul régime de marché (trend baissier). Un test sur marché mixte (trending + ranging + volatile) est indispensable avant de conclure. Le filtre ranging pourrait réduire le WR à 55-60% en pratique si le signal est moins propre sur un trend haussier.

---

## Ordre d'implémentation

```
SEMAINE 1 ✅ TERMINÉ (2026-04-02)
├── ✅ [P1.1] FlowFeatures: price_return_5s, 10s, 30s
├── ✅ [P1.2] MfdpStrategy: refactorer dir_score (OFI → momentum)
├── ✅ [P1.3] RegimeEngine: ajouter RangingMarket
├── ✅ [P2.1] max_hold_s: 600 → 45
├── ✅ [P2.2] Pullback: timeouts indépendants par phase (bug fix)
├── ✅ [P2.3] pullback_min_move_bps: 3.0 → 1.5
├── ✅ [P3.1] Per-coin signal quota (fenêtre 10min)
├── ✅ [P4.1] Script research/scripts/analyze_obi_levels.py
└── [TEST] ⏳ Dry-run 8h — mesurer: diversité coins, pullback
          completion rate, holds <60s, WR vs baseline,
          RangingMarket visible dans logs

PROCHAINE ÉTAPE (~2026-04-05, quand ≥3j de données multi-level)
├── [P4.2] Lancer analyze_obi_levels.py sur les données L2
├── [P4.3] Décision intégration OBI (critère : 2× corr_L1)
└── [P3.2] Cooldown adaptatif SL_HIT vs max_hold

MOYEN TERME (≥2 semaines de dry-run stable)
├── [P5.1] Liquidation map (Python prototype)
├── [P5.2] Funding rate filtre macro
└── [GO/NO-GO live] Décision sur base des métriques dry-run
```

---

## Critères de validation avant passage en live

| Critère | Seuil minimum | Mesure |
|---------|--------------|--------|
| Win Rate dry-run | ≥ 50% | `bot_status.win_rate_pct` |
| P&L/trade net (après spread simulé) | > 0 | `total_pnl_usd / total_trades` |
| Max drawdown session | < 5% | `drawdown_pct` dans api-state |
| Diversité coins | ≥ 3 coins différents sur 4h | analyse journal |
| Concentration coin max | < 50% du flow | analyse journal |
| WR sur ≥ 50 trades | ≥ 50% | statistique fiable |
| 0 bug critique (position orpheline, SL manquant) | obligatoire | scan logs |
| Régime RangingMarket filtré correctement | visible dans logs | `regime → RangingMarket` |

---

## Fichiers modifiés

### Phase 1 ✅
- `src/features/flow_features.rs` — `price_return_5s/10s/30s` + `compute_price_return()`
- `src/strategy/mfdp.rs` — `compute_direction_score()` refactoré, gate `RangingMarket`
- `src/config/settings.rs` — `w_pr5s`, `w_pr10s`, `w_depth_imb`, `trending_min_bps`
- `src/regime/engine.rs` — `Regime::RangingMarket`, classification `|pr30s| < trending_min_bps`
- `config/default.toml` — nouveaux poids, `sl_min_bps=12.0`

### Phase 2 ✅
- `src/config/settings.rs` — `pullback_wait_move_s`, `pullback_wait_retrace_s` (suppression `max_wait_pullback_s`)
- `src/strategy/pullback.rs` — `wait_move_ms` + `wait_retrace_ms` indépendants, reset `expires_at` à la transition
- `src/main.rs` — construction `PullbackSettings` mise à jour
- `config/default.toml` — `max_hold_s=45`, `pullback_min_move_bps=1.5`, `pullback_wait_move_s=20`, `pullback_wait_retrace_s=20`

### Phase 3 (partielle) ✅
- `src/config/settings.rs` — `max_signals_per_coin_10min` dans `RiskSettings`
- `src/main.rs` — `coin_signal_timestamps`, rolling window 10min
- `config/default.toml` — `max_signals_per_coin_10min=6`

### Phase 4.1 ✅
- `research/scripts/analyze_obi_levels.py` — créé (critère 2× corr_L1 intégré)

### Enregistrement données (prérequis Phase 4) ✅
- `src/market_data/recorder.rs` — `bid_levels[10]` + `ask_levels[10]` dans `BookRecord`

---

## Historique

| Date | Version | Changement |
|------|---------|-----------|
| 2026-04-01 | V1.0 | MFDP V1 opérationnelle. Session 1 : 28 trades, WR 39%, P&L −$90 |
| 2026-04-02 | V1.1 | Fix WS reconnect (P0), fix DoNotTrade stuck. Session 2 : 5 trades, WR 0%, P&L −$15 |
| 2026-04-02 | V1.2 | Recorder: ajout bid_levels/ask_levels[10] pour analyse OBI multi-level |
| 2026-04-02 | V2.0 | **Implémentation complète Phases 1-3 + script OBI** : pivot OFI → momentum (pr5s), RangingMarket regime, max_hold 45s, pullback timeouts indépendants, quota signal par coin. Build propre. |

---

*Plan établi sur base d'analyse empirique : 240 866 trades ETH, 132 896 snapshots L2, 20h, 12 coins. Tests statistiques : corrélation de Pearson, simulation momentum avec spread réel.*
