# Evolutions — Stratégies Flat Market pour gbot

> Patterns de trading exploitables en marché ranging/flat, ordonnés par priorité.
> Pour chaque évolution : mécanisme, implémentation concrète dans gbot, contraintes Hyperliquid, et métriques de validation.

---

## Contraintes Hyperliquid (vérifiées dans le code)

| Contrainte | Valeur | Impact |
|-----------|--------|--------|
| Position par coin | **1 seule (net)** — long OU short, pas les deux | Bloque market-making, grid hedgé |
| Ordres pending par coin | **Multiples autorisés** (pas de limite exchange) | Grid possible via ordres multiples |
| Types d'ordres | ALO, GTC limit, trigger (SL/TP), IOC | Pas d'iceberg/TWAP natif |
| Fees maker | **-1.5 bps** (rebate) | Round-trip maker/maker = 3 bps |
| Fees taker | **+4.5 bps** | Round-trip maker/taker = 6 bps |
| Rate limit | 1200 weight/min, ordres = 1 weight chacun | ~1200 ordres/min max théorique |
| Min notional | **$11 USD** | Pas de micro-trades |
| Max coins WS | ~10 coins stables (20 subs) | Pas de scanning 50 coins |

### Contraintes gbot actuelles (code)

| Contrainte | Fichier | Ligne | Impact |
|-----------|---------|-------|--------|
| 1 position/coin | `position_manager.rs` | `HashMap<String, OpenPosition>` | Bloque grid multi-position |
| Pas d'indicateurs TA | — | RSI, BB, ATR, MA absents | Doit utiliser les features microstructure existantes |
| Regime bloque en flat | `regime/engine.rs:106` | `RangingMarket` → `NoTrade` | Le regime doit autoriser les entrées en flat |
| FSM 10 états | `order_manager.rs` | 1 workflow par coin | Grid nécessite un workflow différent |
| SL min 8 bps | `default.toml:122` | Plancher de SL | Scalping ultra-serré impossible |

---

## P0 — Quick wins (jours, pas de refactoring)

> **RÉSULTAT : ABANDONNÉ** — EVO-1/2/3 implémentés et testés en backtest sur 4 jours (01-04 avril 2026, 13 coins).
> Résultat : 6491 trades, WR 10.5%, P&L **-$6785**, Max DD 67.9%, fee drag 242%.
> Tous les coins négatifs. Le flat market n'a pas assez d'amplitude pour couvrir les fees
> (TP=6bps vs fees=3bps round-trip, avg MFE=1.46bps). Code entièrement reverté.

### EVO-1 : Mode Mean-Reversion sur RangingMarket

**Objectif** : Au lieu de bloquer toutes les entrées quand `|pr30s| < 5 bps`, exploiter les oscillations autour de la moyenne en inversant la logique directionnelle.

**Mécanisme** : En marché flat, le prix oscille autour d'un point d'équilibre. Quand les features microstructure indiquent un extrême (prix éloigné du micro_price, profondeur déséquilibrée, vol spike), entrer en sens inverse pour capturer le retour à la moyenne.

**Entrée** : Score mean-reversion > seuil quand regime = `RangingMarket`
**Sortie** : Retour au mid (micro_price_vs_mid_bps → 0) ou SL serré
**SL/TP** : SL = 8 bps, TP = 6-8 bps (RR ~1:1, compense par WR élevé attendu ~60%+)

**Implémentation** :

1. **`regime/engine.rs`** — Splitter `RangingMarket` en deux sous-états :
   ```rust
   // Nouveau
   RangingMarket,       // flat, pas de signal mean-reversion → NoTrade
   RangingMeanRevert,   // flat + conditions mean-reversion réunies → entrée autorisée
   ```
   Conditions pour `RangingMeanRevert` :
   - `|pr30s| < trending_min_bps` (flat confirmé)
   - `|micro_price_vs_mid_bps| > 1.5` OU `|imbalance_weighted| > 0.6` (extrême détecté)
   - `realized_vol_10s > realized_vol_30s * 0.5` (assez de mouvement pour un retour)
   - `spread_bps < 4.0` (spread pas trop large pour le profit cible)
   - `toxicity_proxy < 0.5` (pas de flow toxique)

2. **`strategy/mfdp.rs`** — Ajouter branche `RangingMeanRevert` dans `evaluate()` :
   ```rust
   Regime::RangingMeanRevert => {
       // Inverser le score directionnel pour mean-reversion
       let mr_score = self.compute_mean_reversion_score(features);
       // Entrée en direction OPPOSÉE à la déviation
       // Si micro_price > mid → le book pousse vers le haut → short (retour)
       // Si micro_price < mid → le book pousse vers le bas → long (retour)
   }
   ```

3. **`strategy/mfdp.rs`** — Nouvelle méthode `compute_mean_reversion_score()` :
   ```rust
   fn compute_mean_reversion_score(&self, features: &CoinFeatures) -> f64 {
       // Déviation du micro_price (signal principal en flat)
       let micro_dev = features.book.micro_price_vs_mid_bps / 3.0; // normalisé
       // Imbalance du book (pression qui va se résorber)
       let imb = features.book.imbalance_weighted;
       // VAMP déviation
       let vamp_dev = features.book.vamp_signal_bps / 3.0;
       // Vol spike (retour à la moyenne de la vol)
       let vol_spike = (features.flow.vol_ratio - 1.0).max(0.0) / 2.0;

       // Score signé : positif = long (retour vers le haut), négatif = short
       // En mean-reversion : on INVERSE — si tout pointe vers le haut, on short
       -(0.35 * micro_dev + 0.25 * imb + 0.25 * vamp_dev + 0.15 * vol_spike)
   }
   ```

4. **`config/default.toml`** — Nouveaux paramètres :
   ```toml
   [strategy.mean_reversion]
   enabled = true
   mr_threshold = 0.40              # seuil score MR (plus bas que directionnel car WR attendu plus haut)
   mr_sl_bps = 8.0                  # SL adapté (identique pour commencer)
   mr_tp_bps = 6.0                  # TP plus court (retour au mid, pas de trend)
   mr_max_hold_s = 30               # hold plus court (le retour est rapide ou ne vient pas)
   mr_min_micro_dev_bps = 1.5       # déviation minimum pour considérer un extrême
   mr_min_imbalance = 0.4           # imbalance minimum
   mr_cooldown_s = 60               # cooldown plus court (signaux plus fréquents en range)
   ```

5. **`strategy/signal.rs`** — Pas de nouveau Intent nécessaire. `PlacePassiveEntry` convient, le SL/TP sont déjà dynamiques. Marquer le signal comme `mean_reversion: true` dans le recording pour l'analyse.

6. **Pullback** : Désactiver le pullback tracker pour les entrées MR (le pullback est un concept trending). Entrée directe en ALO au best bid/ask.

**Contrainte Hyperliquid** : Aucune — même workflow qu'un trade directionnel normal (1 ALO entry, 1 trigger SL, 1 ALO TP).

**Ce qu'il faut analyser pour valider** :
- [ ] **Backtester** : Rejouer les sessions flat (Session 4, 6) avec la logique MR. Comparer P&L vs `NoTrade` actuel
- [ ] **Distribution micro_price_vs_mid_bps en flat** : Vérifier que la déviation dépasse 1.5 bps suffisamment souvent pour générer des signaux (analyser les données L2 existantes dans `data/l2/`)
- [ ] **Autocorrélation micro_price → retour au mid** : Mesurer le taux de retour du micro_price vers 0 après un extrême, et en combien de temps (half-life). Si half-life > 30s → TP sera rarement touché
- [ ] **Win rate simulé** : Sur les données historiques, quand `|micro_price_vs_mid_bps| > 1.5` en RangingMarket, est-ce que le prix revient effectivement vers le mid dans les 30s suivantes ?
- [ ] **Impact des fees** : Avec TP=6 bps et fees=3 bps (maker/maker), le profit net est 3 bps/trade. Le WR breakeven est ~57% (fees incluses). Est-ce atteignable ?

**Résultat (2026-04-04)** : ❌ ABANDONNÉ
- Backtest 4 jours (01-04/04), 13 coins : 6491 trades, WR=10.5%, P&L=-$6785, DD=67.9%
- Fee drag 242% — les fees mangent 2.4× le profit brut
- Avg MFE=1.46bps vs TP=6bps — le prix ne bouge jamais assez pour atteindre le TP
- Breakeven WR nécessaire ~57%, obtenu ~10% — non viable

---

### EVO-2 : Squeeze Detection (filtre de régime amélioré)

**Objectif** : Améliorer la détection du régime `RangingMarket` avec un indicateur de compression de volatilité. Détecter les breakouts imminents pour couper les stratégies MR avant qu'elles prennent un SL sur le breakout.

**Mécanisme** : Quand la volatilité courte (3-10s) se compresse par rapport à la volatilité longue (30s), le marché accumule de l'énergie. La compression prolongée précède souvent un breakout. Inversement, une compression stable = range confirmé.

**Implémentation** :

1. **`features/flow_features.rs`** — Ajouter des features de compression :
   ```rust
   // Dans FlowFeatures
   pub vol_compression: f64,     // realized_vol_3s / realized_vol_30s (< 0.5 = compressé)
   pub vol_expanding: bool,      // vol_compression augmente sur les 5 derniers ticks
   ```

2. **`features/engine.rs`** — Calculer `vol_compression` :
   ```rust
   // Déjà disponible via vol_ratio (= vol_3s / vol_30s)
   // Juste renommer/alias pour clarté + ajouter le tracking de tendance
   features.flow.vol_compression = features.flow.realized_vol_3s
       / features.flow.realized_vol_30s.max(1e-10);
   ```
   Pour `vol_expanding`, maintenir un ring buffer de 5 valeurs de `vol_compression` et vérifier si la pente est positive.

3. **`regime/engine.rs`** — Ajouter un garde dans `RangingMeanRevert` :
   ```rust
   // Ne PAS entrer en mean-reversion si la vol est en expansion
   // (breakout probable → le retour au mid ne viendra pas)
   if features.flow.vol_expanding && features.flow.vol_compression > 1.5 {
       return Regime::RangingMarket; // flat mais breakout imminent → pas de MR
   }
   ```

4. **`config/default.toml`** :
   ```toml
   [regime]
   squeeze_vol_compression_max = 1.5   # vol_compression > 1.5 + expanding = breakout imminent
   squeeze_lookback_ticks = 5          # nombre de ticks pour détecter expansion
   ```

**Contrainte Hyperliquid** : Aucune — c'est un filtre purement logique, pas d'ordre.

**Ce qu'il faut analyser pour valider** :
- [ ] **Taux de breakout après squeeze** : Sur les données historiques, quand `vol_compression` passe de < 0.5 à > 1.5, est-ce qu'un breakout (|pr30s| > 10 bps) suit dans les 60s ?
- [ ] **Réduction des faux signaux MR** : Combien de trades MR auraient été évités par le filtre squeeze, et parmi ceux-ci combien auraient touché le SL ?
- [ ] **Corrélation vol_compression / SL rate** : Comparer le SL rate des trades MR avec et sans filtre squeeze

**Résultat (2026-04-04)** : ❌ ABANDONNÉ avec EVO-1. Le filtre squeeze était fonctionnel mais inutile car la stratégie MR sous-jacente est non viable.

---

### EVO-3 : Vol Spike Mean Reversion

**Objectif** : Quand un micro-événement cause un spike de volatilité dans un marché par ailleurs flat, exploiter le retour à la normale. Le prix bouge brièvement dans une direction puis revient.

**Mécanisme** : En marché flat, `vol_ratio` spike > 2x signale un micro-événement (grosse trade, sweep). La direction du spike (via `price_return_5s`) indique où le prix est allé. Entrer en sens inverse pour capter le retour.

**Entrée** : `vol_ratio > 2.0` ET regime = `RangingMarket` ET `|price_return_5s| > 2 bps`
**Direction** : Inverse de `price_return_5s` (si pr5s > 0 → short, si pr5s < 0 → long)
**Sortie** : Retour de `price_return_5s` vers 0 OU max_hold timeout
**SL** : 10 bps (le spike peut continuer)
**TP** : 5-8 bps (retour partiel suffit)

**Implémentation** :

1. **Intégrer dans EVO-1** comme sous-cas de `compute_mean_reversion_score()` :
   ```rust
   // Dans compute_mean_reversion_score, ajouter le composant vol_spike
   let vol_spike_signal = if features.flow.vol_ratio > 2.0
       && features.flow.price_return_5s.abs() > 2.0 {
       // Signal fort : prix a bougé sur un spike → fade
       -features.flow.price_return_5s.signum() * 0.8
   } else {
       0.0
   };
   // Incorporer dans le score total avec un poids élevé
   ```

2. **Ou alternativement**, comme signal standalone avec priorité sur le MR classique :
   - Quand vol_spike_signal est actif, utiliser un SL/TP dédié (plus large car le spike peut être violent)
   - `mr_vol_spike_sl_bps = 10.0`, `mr_vol_spike_tp_bps = 6.0`

3. **Cooldown spécifique** : Après un trade vol-spike, attendre que `vol_ratio` redescende < 1.5 avant de re-trader (évite de re-entrer dans le même spike).

**Contrainte Hyperliquid** : Aucune — même workflow que EVO-1.

**Ce qu'il faut analyser pour valider** :
- [ ] **Fréquence des vol spikes en flat** : Sur les données existantes, combien de fois par heure `vol_ratio > 2.0` ET `|pr5s| > 2 bps` en `RangingMarket` ?
- [ ] **Taux de retour post-spike** : Quand pr5s spike > 2 bps en flat, est-ce que le prix revient dans les 30s ? Mesurer % de retour et half-life
- [ ] **Distinction spike vs breakout** : Un vol_spike en flat qui NE revient PAS = début de breakout. Mesurer le ratio (retour / continuation) pour calibrer le SL
- [ ] **P&L simulé** : Backtester sur sessions flat (Session 4, 6) avec la logique vol-spike fade

**Résultat (2026-04-04)** : ❌ ABANDONNÉ avec EVO-1. Le vol-spike fade (composante du MR score avec poids adaptatif 85%) était le principal driver de signaux, mais le MFE post-spike est insuffisant pour couvrir les fees.

---

## P1 — Effort modéré (1-2 semaines)

### EVO-4 : False Breakout Fade

**Objectif** : Quand le prix casse brièvement un support/résistance dynamique puis réintègre le range, entrer en sens inverse du breakout. Les faux breakouts sont fréquents en marché flat.

**Mécanisme** : Tracker le min/max prix sur une fenêtre glissante (ex: 60s). Quand le prix dépasse le max (ou casse sous le min) puis revient à l'intérieur, c'est un faux breakout. Confirmation par faible volume (pas de conviction derrière le breakout).

**Entrée** :
- Prix a dépassé le max_60s de > 1 bps puis est revenu en dessous
- `trade_intensity` sur le breakout < moyenne (pas de gros volume)
- `large_trade_ratio` < 0.3 (pas de grosse trade directionnelle)
- Regime = `RangingMarket`

**Direction** : Opposée au breakout (breakout haussier raté → short, baissier raté → long)
**SL** : 1 bps au-delà du point de breakout
**TP** : Retour au milieu du range (mid_60s)

**Implémentation** :

1. **`features/flow_features.rs`** — Ajouter tracking des extrêmes :
   ```rust
   pub price_high_60s: f64,      // max prix sur 60s
   pub price_low_60s: f64,       // min prix sur 60s
   pub price_range_60s_bps: f64, // (high - low) / mid × 10000
   pub price_mid_60s: f64,       // (high + low) / 2
   ```
   Calculer depuis le trade tape existant (on a déjà les 1000 derniers trades avec timestamps).

2. **`features/flow_features.rs`** — Détecter le faux breakout :
   ```rust
   pub struct BreakoutState {
       pub breakout_detected: bool,
       pub breakout_direction: Option<Direction>, // Up ou Down
       pub breakout_price: f64,                   // prix au moment du breakout
       pub breakout_faded: bool,                  // prix est revenu dans le range
       pub breakout_volume_weak: bool,            // faible volume pendant le breakout
   }
   ```

3. **`strategy/mfdp.rs`** — Nouveau path dans `evaluate()` quand `breakout_faded = true` :
   ```rust
   if features.flow.breakout_state.breakout_faded
       && features.flow.breakout_state.breakout_volume_weak
       && regime == Regime::RangingMarket {
       // Fade le breakout : entrer en direction opposée
       let direction = features.flow.breakout_state.breakout_direction
           .unwrap().opposite();
       // TP = retour au mid_60s
       // SL = breakout_price + 1 bps buffer
   }
   ```

4. **Intégration avec le PullbackTracker** : Le PullbackTracker existant attend déjà un retrace après un move. On peut le réutiliser :
   - Le "move" = le faux breakout
   - Le "retrace" = le début du retour dans le range
   - Adapter les seuils (`pullback_min_move_bps` dynamique basé sur `price_range_60s_bps`)

5. **`config/default.toml`** :
   ```toml
   [strategy.false_breakout]
   enabled = true
   lookback_s = 60                  # fenêtre pour calculer high/low
   breakout_threshold_bps = 1.0     # dépassement min pour considérer un breakout
   fade_confirm_bps = 0.5           # retour min dans le range pour confirmer le fade
   max_volume_ratio = 0.3           # large_trade_ratio max pendant le breakout
   sl_buffer_bps = 1.0              # SL au-delà du point de breakout
   tp_target = "mid_range"          # retour au milieu du range
   ```

**Contrainte Hyperliquid** : Aucune — même workflow standard (1 position/coin).

**Ce qu'il faut analyser pour valider** :
- [ ] **Fréquence des faux breakouts en flat** : Sur données historiques, combien de fois le prix dépasse high_60s/low_60s puis revient ? Ratio faux breakouts / vrais breakouts ?
- [ ] **Volume profile** : Est-ce que les faux breakouts ont systématiquement moins de volume que les vrais ? Quel seuil `large_trade_ratio` discrimine le mieux ?
- [ ] **Profondeur du retour** : Quand le prix fade, jusqu'où revient-il ? (mid_60s ? Au-delà ?)
- [ ] **Timing** : Combien de temps entre le breakout et le retour dans le range ? Si > 30s, le max_hold_s de 45s laisse peu de marge

---

### EVO-5 : Bandes de Volatilité (Bollinger-like via realized_vol)

**Objectif** : Utiliser la volatilité réalisée déjà calculée pour créer des bandes dynamiques autour du prix moyen. En flat, le prix rebondit entre ces bandes → mean reversion.

**Mécanisme** : Au lieu de Bollinger Bands classiques (SMA + σ), utiliser :
- Centre = prix moyen pondéré sur 30s (déjà dans le trade tape)
- Bande haute = centre + `K × realized_vol_30s × prix`
- Bande basse = centre - `K × realized_vol_30s × prix`

Quand le prix touche une bande → entrée en sens inverse.

**Implémentation** :

1. **`features/flow_features.rs`** — Ajouter les bandes :
   ```rust
   pub mean_price_30s: f64,         // prix moyen pondéré sur 30s (VWAP-like)
   pub upper_band_bps: f64,         // distance prix actuel → bande haute (en bps)
   pub lower_band_bps: f64,         // distance prix actuel → bande basse (en bps)
   pub band_width_bps: f64,         // largeur totale des bandes
   pub price_position_in_band: f64, // 0.0 = bande basse, 1.0 = bande haute
   ```
   Calculer :
   ```rust
   let mean = weighted_avg_price_30s(tape); // somme(price × size) / somme(size)
   let band_dist = K * realized_vol_30s * mean; // K configurable, ex: 2.0
   let upper = mean + band_dist;
   let lower = mean - band_dist;
   features.flow.price_position_in_band = (current_price - lower) / (upper - lower);
   ```

2. **Intégrer dans EVO-1** `compute_mean_reversion_score()` :
   ```rust
   // price_position_in_band: 0 = bande basse (long), 1 = bande haute (short)
   // Transformer en score MR : 0.5 = neutre, 0 = long fort, 1 = short fort
   let band_signal = (features.flow.price_position_in_band - 0.5) * 2.0; // [-1, +1]
   // Négatif = prix en bas → long, positif = prix en haut → short
   // En mean-reversion on INVERSE : band_signal positif → short (retour vers le centre)
   ```
   Ajouter comme composant dans le score MR avec poids ~0.30.

3. **`config/default.toml`** :
   ```toml
   [strategy.vol_bands]
   band_multiplier = 2.0          # K : nombre de σ pour les bandes
   band_entry_threshold = 0.80    # price_position > 0.80 ou < 0.20 pour signal
   band_lookback_s = 30           # fenêtre pour le prix moyen
   ```

**Contrainte Hyperliquid** : Aucune.

**Ce qu'il faut analyser pour valider** :
- [ ] **Calibrage du K** : Avec K=2.0, combien de % du temps le prix est hors bandes en flat ? Trop bas = trop de signaux (bruit), trop haut = pas assez de signaux
- [ ] **Taux de rebond** : Quand `price_position > 0.80`, est-ce que le prix revient vers 0.50 dans les 30s ? Quel % ?
- [ ] **Comparaison avec micro_price** : Est-ce que les bandes de vol apportent un signal additionnel par rapport au `micro_price_vs_mid_bps` déjà utilisé dans EVO-1 ? Corrélation entre les deux ?
- [ ] **Band width comme filtre** : Quand `band_width_bps < 3 bps`, le range est trop serré pour les fees (3 bps maker/maker). Filtrer ces cas

---

### EVO-6 : Z-Score Ornstein-Uhlenbeck

**Objectif** : Modéliser le prix comme un processus mean-reverting (OU) et utiliser le z-score pour timer les entrées MR de manière statistiquement rigoureuse.

**Mécanisme** : Le modèle OU postule que le prix revient vers sa moyenne à un taux θ. Le z-score = (prix - μ) / σ mesure l'écart standardisé. Entrée quand |z| > seuil, sortie quand z → 0.

**Implémentation** :

1. **Nouveau module `features/ou_model.rs`** :
   ```rust
   pub struct OUEstimator {
       prices: VecDeque<f64>,       // prix récents (rolling window)
       timestamps: VecDeque<i64>,
       lookback: usize,             // ex: 200 ticks
       // Paramètres estimés
       pub theta: f64,              // vitesse de mean-reversion
       pub mu: f64,                 // moyenne long-terme
       pub sigma: f64,              // volatilité du processus
       pub z_score: f64,            // (prix_actuel - mu) / sigma
       pub half_life_s: f64,        // -ln(2) / ln(1 - theta × dt)
   }
   ```

2. **Estimation des paramètres** (rolling, chaque N ticks) :
   ```rust
   impl OUEstimator {
       fn estimate(&mut self) {
           // Régression linéaire : ΔX = a + b × X_{t-1}
           // theta = -b / dt
           // mu = -a / b
           // sigma = std_dev(résidus) / sqrt(dt)
           // ... (AR(1) regression sur les prix)
       }

       fn update(&mut self, price: f64, timestamp: i64) {
           self.prices.push_back(price);
           self.timestamps.push_back(timestamp);
           if self.prices.len() > self.lookback { self.prices.pop_front(); }
           if self.prices.len() >= 50 { // min samples
               self.estimate();
               self.z_score = (price - self.mu) / self.sigma.max(1e-10);
               let dt = /* avg dt between ticks */;
               self.half_life_s = -(2.0_f64.ln()) / (1.0 - self.theta * dt).ln();
           }
       }
   }
   ```

3. **Intégration** : Instancier un `OUEstimator` par coin dans `FeatureEngine`. Exposer `z_score` et `half_life` dans `FlowFeatures`.

4. **Entrée MR basée sur z-score** : Dans EVO-1, remplacer ou combiner avec le score existant :
   ```rust
   // z_score > 2.0 → prix très au-dessus de la moyenne → short
   // z_score < -2.0 → prix très en dessous → long
   let ou_signal = -features.flow.z_score.clamp(-3.0, 3.0) / 3.0; // normalisé [-1, +1]
   ```

5. **Filtrage par half-life** : Si `half_life_s > max_hold_s`, le retour à la moyenne est trop lent pour notre fenêtre de trade → ne pas entrer.

6. **`config/default.toml`** :
   ```toml
   [strategy.ou_model]
   enabled = false                # désactivé par défaut, activer après validation
   lookback_ticks = 200
   z_entry_threshold = 2.0        # |z| > 2 pour entrer
   z_exit_threshold = 0.5         # |z| < 0.5 pour sortir
   max_half_life_s = 30           # ignorer si half-life > max_hold_s
   min_samples = 50               # minimum de ticks pour estimer
   refit_every_ticks = 20
   ```

**Contrainte Hyperliquid** : Aucune — même workflow.

**Ce qu'il faut analyser pour valider** :
- [ ] **Stationnarité** : Sur les fenêtres RangingMarket, est-ce que le test ADF rejette la racine unitaire (p < 0.05) ? Si non, le modèle OU n'est pas applicable
- [ ] **Stabilité des paramètres** : θ et μ sont-ils stables sur 200 ticks en flat, ou dérivent-ils trop vite ?
- [ ] **Half-life distribution** : Quelle est la distribution des half-life en flat ? Si median > 30s, les trades ne closeront pas dans le max_hold
- [ ] **Z-score predictive power** : Corrélation entre z_score et le return futur à horizon half-life. Doit être significativement négative (mean-reversion)
- [ ] **Implémentation Rust** : Vérifier les crates disponibles (`nalgebra`, `ndarray`, `statrs`) pour la régression AR(1). Sinon implémenter manuellement (formule simple)

---

## P2 — Effort significatif (2-4 semaines)

### EVO-7 : Grid Trading Simplifié

**Objectif** : Placer des ordres ALO à intervalles réguliers dans un range confirmé. Chaque fill capture un spread fixe.

**Mécanisme** : Définir N niveaux de prix dans le range [low, high]. Placer des buy ALO aux niveaux inférieurs et des sell ALO aux niveaux supérieurs. Quand un buy fill, placer un sell un niveau au-dessus. Et inversement.

**Contraintes Hyperliquid critiques** :
- **1 position nette par coin** : Le grid ne peut PAS avoir un buy ET un sell ouverts simultanément comme positions. Mais il PEUT avoir **plusieurs ordres pending** (non fillés).
- **Impact concret** : Si un buy fill → position = long. On ne peut pas aussi avoir un sell fill qui ouvrirait un short. Le sell doit être `reduce_only` (fermer le long) OU on accumule une position long de plus en plus grosse.
- **Modèle viable** : Grid unidirectionnel ou grid avec position nette :
  - Buy aux niveaux bas → position long grandit
  - Sell (reduce_only) quand prix monte → position shrink
  - Net = on accumule en bas, on prend profit en haut

**Implémentation** :

1. **Nouveau module `strategy/grid.rs`** :
   ```rust
   pub struct GridStrategy {
       levels: Vec<GridLevel>,
       range_high: f64,
       range_low: f64,
       grid_spacing_bps: f64,
       active: bool,
   }

   struct GridLevel {
       price: f64,
       has_pending_buy: bool,
       has_pending_sell: bool,
       buy_oid: Option<String>,
       sell_oid: Option<String>,
   }
   ```

2. **Refactoring requis** :
   - **`execution/order_manager.rs`** : Actuellement 1 FSM par coin. Il faut supporter N ordres pending par coin (un par niveau de grid). Gros refactoring de la state machine.
   - **`execution/position_manager.rs`** : Tracker les fills partiels et la position nette au lieu d'une position unique avec SL/TP fixe.
   - **`risk/manager.rs`** : Adapter les contrôles pour le grid : max exposure = somme de tous les niveaux, pas juste 1 position.

3. **Range detection** : Utiliser EVO-5 (bandes de vol) pour définir `range_high` et `range_low`. Recalibrer toutes les N minutes.

4. **`config/default.toml`** :
   ```toml
   [strategy.grid]
   enabled = false
   grid_levels = 10                 # nombre de niveaux
   grid_spacing_bps = 3.0           # espacement entre niveaux (> fees)
   grid_max_exposure_usd = 1000.0   # exposition max totale du grid
   grid_recalibrate_s = 300         # recalibrer les niveaux toutes les 5 min
   grid_sl_below_range_bps = 10.0   # SL global si prix casse sous le range
   ```

**Contrainte Hyperliquid** :
- Les ordres multiples pending sont autorisés
- Mais la position nette est unique → le grid fonctionne en mode "accumulate low / distribute high" et non en mode classique bid/ask simultané
- Min notional $11 → avec 10 niveaux, minimum $110 de capital alloué au grid
- Rate limit : 10 niveaux × 2 ordres = 20 ordres/recalibrage = 20 weight (largement dans les limites)

**Ce qu'il faut analyser pour valider** :
- [ ] **Simulation grid** : Script Python pour simuler un grid sur les données L2 enregistrées. Mesurer : nombre de cycles complets, P&L par cycle, drawdown max quand le range casse
- [ ] **Grid spacing optimal** : Avec fees maker/maker = 3 bps, le spacing minimum rentable est ~4 bps. Tester 3/4/5/6 bps
- [ ] **Durée des ranges** : Combien de temps les ranges stables durent-ils ? Si < 5 min, pas le temps de remplir assez de niveaux
- [ ] **Risque de breakdown** : Quand le prix casse le range, quelle est la perte typique ? (position accumulée × distance de chute)
- [ ] **Comparaison avec EVO-1** : Grid vs mean-reversion simple — lequel performe mieux en flat ? Le grid nécessite plus de capital et de complexité

---

### EVO-8 : HMM Regime Detection (microservice)

**Objectif** : Remplacer le régime binaire (trending/flat basé sur `|pr30s| < 5 bps`) par un modèle HMM à 3 états (bull/bear/flat) avec probabilités continues.

**Mécanisme** : Un Hidden Markov Model apprend les transitions entre régimes à partir des features observables (returns, vol, volume). Au lieu d'un seuil binaire, on obtient P(flat) ∈ [0, 1].

**Implémentation** :

1. **Microservice Python** (séparé de gbot) :
   ```python
   # Service HTTP/WebSocket qui :
   # 1. Reçoit les features de gbot (pr5s, pr10s, pr30s, vol_ratio, trade_intensity)
   # 2. Maintient un HMM par coin (hmmlearn.GaussianHMM, n_components=3)
   # 3. Publie P(flat), P(bull), P(bear) par coin
   ```

2. **Intégration gbot** — Client HTTP dans `features/engine.rs` :
   ```rust
   // Toutes les N secondes, envoyer features au microservice
   // Recevoir les probabilités de régime
   // Utiliser P(flat) > 0.7 au lieu de |pr30s| < 5bps
   ```

3. **Training** : Sur les données historiques L2/trades déjà enregistrées dans `data/`.

4. **Fallback** : Si le microservice est down → retomber sur le régime heuristique actuel.

**Contrainte Hyperliquid** : Aucune — c'est un filtre côté bot.

**Ce qu'il faut analyser pour valider** :
- [ ] **Qualité de classification** : Entraîner HMM sur données historiques, vérifier que les 3 états correspondent bien à bull/bear/flat visuellement
- [ ] **Stabilité des transitions** : Le HMM switch-t-il trop fréquemment (whipsaw) ou est-il stable ?
- [ ] **Latence** : Le microservice HTTP ajoute-t-il trop de latence (> 100ms problématique pour le scalp) ?
- [ ] **Amélioration vs heuristique** : Backtester les deux méthodes sur les mêmes données — le HMM détecte-t-il les transitions trending→flat plus tôt ou plus tard que le seuil 5 bps ?
- [ ] **Faux positifs** : Le HMM classe-t-il parfois un breakout comme "flat" ? (risque de MR sur un mouvement directionnel)
- [ ] **Complexité opérationnelle** : Maintenir un service Python séparé + communication inter-process. Vaut-il le coup vs améliorer l'heuristique ?

---

### EVO-9 : Scalping Range-Bound (SL/TP ultra-serrés)

**Objectif** : En flat confirmé, utiliser des SL/TP très serrés avec un grand nombre de trades pour accumuler de petits profits. Compenser la faible amplitude par le volume.

**Mécanisme** : Quand le range est confirmé (EVO-2 squeeze stable), entrer rapidement sur des signaux MR avec :
- SL = 5 bps (normalement interdit car < 2× fees, mais ici TP maker = 3 bps fees round-trip)
- TP = 4 bps (juste au-dessus des fees)
- Profit net/trade = ~1 bps
- Volume compensatoire : 20+ trades/heure au lieu de 5

**Problème connu** : Session 4 a prouvé que SL=5 bps → 51% SL hit. Mais c'était en mode directionnel. En mode MR avec confirmation microstructure (EVO-1), le WR pourrait être supérieur.

**Implémentation** :

1. **Modification `config/default.toml`** :
   ```toml
   [strategy.scalp_range]
   enabled = false
   sl_bps = 5.0                    # attention : < 2× taker fees (6bps)
   tp_bps = 4.0                    # juste au-dessus des maker fees (3bps)
   # OBLIGATION : TP ET SL doivent être maker (ALO)
   # → nécessite SL maker (pas trigger) pour que fees = 3bps round-trip
   max_hold_s = 15                 # très court
   cooldown_s = 30                 # réduit pour plus de trades
   max_signals_per_coin_10min = 20 # augmenté
   ```

2. **SL maker au lieu de trigger** : Actuellement le SL est un trigger order (taker, 4.5 bps). Pour que le scalping ultra-serré soit viable, il faut un SL passif (ALO). Risque : le SL ALO peut ne pas fill si le prix gape.

3. **Nouveau path dans le OrderManager** pour le mode scalp : skip pullback, entrée directe, gestion simplifiée.

**Contrainte Hyperliquid** :
- ALO pour SL : Possible mais risqué (pas de fill garanti si le prix gape au-delà)
- Rate limit : 20 trades/heure × 3 ordres/trade (entry + TP + SL) = 60 ordres/heure = 1 weight/heure — pas de souci
- Min notional $11 → chaque trade = $11 minimum. Profit = $11 × 1 bps = $0.011/trade. Besoin de levier pour que ça vaille le coup

**Ce qu'il faut analyser pour valider** :
- [ ] **CRITIQUE — WR en mode MR avec SL=5bps** : La question centrale. Si WR < 75%, le scalping ultra-serré est perdant net. Backtester sur les données flat avec la logique MR d'EVO-1
- [ ] **SL maker fill rate** : Sur les données historiques, quand le prix baisse de 5 bps, est-ce que ça arrive en gap (= ALO SL ne fill pas) ou graduellement (= ALO SL fill) ?
- [ ] **Fee sensitivity** : Avec fees maker/maker = 3 bps et TP = 4 bps, le profit net = 1 bps. Si les fees changent de 0.5 bps, le profit tombe à 0.5 bps (division par 2). Très fragile
- [ ] **Session 4 replay** : Rejouer Session 4 avec logique MR + SL=5 bps. Si WR passe de 14% (directionnel) à > 75% (MR), le scalping est viable
- [ ] **Conclusion probable** : Ce mode est très risqué et dépend entièrement du WR MR. Ne l'activer QUE si EVO-1 prouve un WR > 70% en backtest

---

## P3 — Effort majeur ou outil externe (mois)

### EVO-10 : Market-Making (Avellaneda-Stoikov)

**Objectif** : Poster des ordres des deux côtés du spread en permanence, profiter du bid-ask spread.

**Mécanisme** : Le modèle A-S calcule un "reservation price" ajusté par l'inventaire et la volatilité, puis poste des ordres symétriques autour.

**Contrainte Hyperliquid BLOQUANTE** :
- **1 position nette par coin** → Impossible de maintenir une position long ET short simultanément
- Le market-making classique nécessite d'être neutre (acheter ET vendre en permanence)
- Avec Hyperliquid, quand un buy fill → position long. Le prochain sell ferme le long au lieu d'ouvrir un short indépendant
- **Résultat** : Le bot fait effectivement du "flip trading" (long → flat → short → flat → ...) plutôt que du vrai market-making

**Viabilité** : Le flip-trading via A-S peut quand même fonctionner mais :
- Le profit est limité au spread capturé MOINS les fees
- Sur Hyperliquid perps, spread BTC ≈ 1-2 bps. Avec fees maker = -1.5 bps (rebate), le profit net ≈ 2.5-3.5 bps par flip
- Nécessite un volume de trades très élevé pour être rentable

**Recommandation** : Utiliser **Hummingbot** (open-source, supporte Hyperliquid) plutôt que de refactorer gbot. Hummingbot implémente déjà A-S avec :
- Inventory skewing
- Dynamic spread adjustment
- Multiple exchange support

**Ce qu'il faut analyser pour valider** :
- [ ] **Spread moyen par coin** : Calculer le spread moyen en bps sur 24h pour chaque coin actif. Si spread < 3 bps → pas viable avec les fees
- [ ] **Test Hummingbot** : Déployer Hummingbot sur Hyperliquid en paper trading. Mesurer P&L sur 48h
- [ ] **Adverse selection** : Mesurer la corrélation entre le fill d'un order maker et le mouvement de prix immédiatement après. Si le prix bouge systématiquement contre le fill → adverse selection élevée → MM non viable

---

### EVO-11 : Grid Trading via Passivbot

**Objectif** : Utiliser passivbot (Python/Rust, open-source) pour le grid trading au lieu de l'implémenter dans gbot.

**Avantages** :
- Passivbot supporte **Hyperliquid** nativement
- Algorithme d'optimisation évolutionnaire inclus (trouve les meilleurs params via backtest)
- "Unstucking mechanism" pour gérer les positions underwater
- Mode "forager" qui sélectionne dynamiquement les coins les plus volatils
- Communauté active, battle-tested

**Implémentation** : Déployer passivbot en parallèle de gbot, sur des coins différents pour éviter les conflits de position.

**Ce qu'il faut analyser pour valider** :
- [ ] **Backtest passivbot** : Lancer l'optimiseur sur les données Hyperliquid. Quels params pour les coins gbot ?
- [ ] **Conflits** : S'assurer que passivbot et gbot ne tradent pas le même coin (sinon conflits de position)
- [ ] **Coexistence** : Les deux bots partagent le même wallet ? Subaccounts différents ?
- [ ] **Comparaison avec EVO-7** : Passivbot grid vs grid maison — quel effort pour quel résultat ?

---

## Résumé — Plan d'action

```
Phase 1 (P0 — cette semaine) :
  ├── EVO-1 : Mean-Reversion sur RangingMarket ← le plus important
  ├── EVO-2 : Squeeze detection (filtre)
  └── EVO-3 : Vol Spike MR (intégré dans EVO-1)

Phase 2 (P1 — semaine prochaine) :
  ├── EVO-4 : False Breakout Fade
  ├── EVO-5 : Bandes de volatilité
  └── EVO-6 : Z-Score OU (si backtest Phase 1 positif)

Phase 3 (P2 — S+2/S+3) :
  ├── EVO-7 : Grid Trading simplifié OU EVO-11 (passivbot)
  ├── EVO-8 : HMM Regime
  └── EVO-9 : Scalping ultra-serré (si EVO-1 WR > 70%)

Phase 4 (P3 — backlog) :
  └── EVO-10 : Market-Making A-S (probablement via Hummingbot)
```

### Première étape critique avant toute implémentation

**Analyser les données flat existantes** (`data/l2/` et `data/trades/`) pour répondre à :
1. Quelle est la distribution de `micro_price_vs_mid_bps` en `RangingMarket` ?
2. Quelle est l'autocorrélation du retour au mid après un extrême ?
3. Quelle est la fréquence des vol spikes en flat ?
4. Quelle est la distribution des `price_position_in_band` ?

Si ces analyses montrent que le prix ne mean-revert PAS en flat sur Hyperliquid (contrairement à la théorie), alors les EVO-1 à EVO-6 sont toutes invalides et il faut se tourner vers le grid (EVO-7/11) ou accepter de ne pas trader en flat.
