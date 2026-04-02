
# Plan détaillé — nouveau bot Hyperliquid 1m / 3m

Version: 2026-03-31
Auteur: ChatGPT
Format: plan d’implémentation détaillé pour un bot de trading **court terme** sur Hyperliquid

---

## 1. Objectif du projet

Construire un bot de trading **production-grade** pour Hyperliquid, spécialisé sur des horizons **1 minute à 3 minutes maximum**, conçu pour rester viable **après frais, slippage, funding et limites de levier**.

Le bot ne doit **pas** être un simple bot “indicateurs sur chandeliers”.
Il doit exploiter la **microstructure** du marché:

- order book
- order flow imbalance
- spread
- micro-price / VAMP
- profondeur locale
- volatilité réalisée très court terme
- positionnement passif en maker quand le contexte est favorable
- sortie agressive uniquement quand le risque augmente

L’idée centrale est:

> **le signal vient du carnet, l’exécution cherche d’abord à être maker, et le taker n’est utilisé qu’en défense ou quand l’avantage statistique est exceptionnellement fort.**

---

## 2. Pourquoi cette stratégie et pas une autre

## 2.1. Contrainte fondamentale d’Hyperliquid

Sur Hyperliquid, les frais perp de base sont non négligeables pour du 1m / 3m:

- taker: **0.045%**
- maker: **0.015%**

Donc, avant même le slippage:

- aller-retour taker/taker ≈ **9 bps**
- aller-retour maker/taker ≈ **6 bps**
- aller-retour maker/maker ≈ **3 bps**

Conséquence directe:

- un bot qui entre et sort au marché tout le temps doit générer un edge brut très élevé juste pour compenser les coûts
- un bot purement indicateurs (RSI/EMA/MACD) sur bougies 1m devient très vite fragile
- les stratégies les plus naturelles sont celles qui:
  - **réduisent les coûts d’exécution**
  - **trient très sévèrement les trades**
  - **utilisent des signaux microstructurels plus rapides que les chandeliers**

## 2.2. Impact du levier limité par actif

Le levier max varie selon l’actif. Il n’est pas uniforme, et il est bien plus faible sur de nombreux actifs que sur les plus grosses paires.
Donc:

- les petits edges en bps comptent beaucoup
- l’idée “je compense une stratégie médiocre par plus de levier” ne tient pas
- le bot doit sélectionner les marchés où:
  - liquidité correcte
  - spread acceptable
  - profondeur suffisante
  - levier suffisant
  - comportement du carnet exploitable

## 2.3. Conclusion stratégique

La meilleure option réaliste n’est **ni**:

- un market making naïf
- un breakout naïf au marché
- un grid bot
- une martingale
- un bot RSI/EMA sur OHLCV

Le meilleur compromis pour un premier bot robuste est:

# **Bot directionnel maker-first, queue-aware, microstructure-driven**

Autrement dit:

1. le bot détecte un biais court terme à partir du carnet et du flow
2. il attend un **petit pullback local**
3. il se positionne en **ALO / post-only**
4. il cherche à sortir:
   - en maker si le contexte reste sain
   - en taker uniquement si le risque augmente ou si le signal se retourne
5. il applique un risk management très strict
6. il n’opère que sur les actifs les plus adaptés

---

## 3. Stratégie choisie

## 3.1. Nom de travail

**Hyperliquid MFDP Bot**
(**M**icrostructure **F**irst **D**irectional **P**ullback)

## 3.2. Philosophie

Le bot n’essaie pas de “prédire les bougies”.
Il essaie de détecter des situations où:

- le flow court terme a une direction probable
- le carnet confirme cette direction
- la volatilité est exploitable mais pas chaotique
- le spread et la profondeur rendent l’exécution possible
- une entrée passive a une bonne probabilité d’être exécutée sans trop d’adverse selection

## 3.3. Type de trade

### Long setup
- OFI positif
- micro-price > mid-price
- pression acheteuse récente
- spread acceptable
- profondeur bid/ask compatible
- volatilité ni trop basse ni explosive
- petit retracement local vers bid / near-bid
- entrée en ALO buy
- sortie partielle ou totale selon scénario

### Short setup
Miroir du long:
- OFI négatif
- micro-price < mid-price
- pression vendeuse récente
- etc.

---

## 4. Ce que le bot fera et ne fera pas

## 4.1. Le bot fera

- écouter le WebSocket Hyperliquid en continu
- maintenir un état local temps réel du marché
- calculer des features microstructurelles en streaming
- classer le contexte de marché par régime
- ouvrir des positions très courtes
- privilégier les entrées maker
- gérer les sorties par règles explicites
- journaliser tout pour backtest / replay / audit

## 4.2. Le bot ne fera pas (V1)

- deep learning en ligne
- RL en production
- arbitrage multi-exchange
- market making symétrique permanent
- trading sur dizaines d’actifs simultanément
- gestion multi-stratégies complexe
- hedging spot/perp
- portfolio margin sophistiqué
- cross-venue smart order routing

---

## 5. Choix du langage et de l’architecture

## 5.1. Langage recommandé pour le moteur live: **Rust**

C’est le meilleur choix ici.

### Pourquoi Rust
- très bon contrôle de la latence
- excellente gestion de la concurrence
- sécurité mémoire forte
- très bon fit pour:
  - WebSockets intensifs
  - traitement événementiel
  - state machines
  - journaling binaire/structuré
- plus robuste qu’un bot Python pour une exécution 24/7
- meilleur contrôle des types numériques, de l’état et des erreurs
- réduit le risque de race conditions ou d’effets de bord silencieux

### Pourquoi pas Python pour le live
Python reste excellent pour:
- recherche
- backtests
- notebooks
- calibration
- analytics

Mais pour un moteur live 1m/3m avec order book et logique d’exécution fine:
- le GIL
- la fragilité opérationnelle
- la gestion des tâches concurrentes
- la dette de performance
rendent Python moins adapté comme cœur de production.

## 5.2. Choix d’architecture: **modular monolith event-driven**

### Pourquoi pas des microservices au début
Sur un bot de trading court terme, les microservices ajoutent:
- latence réseau
- complexité de déploiement
- multiplications des pannes possibles
- observabilité plus dure
- synchronisation plus délicate

Au début, mieux vaut **un seul process principal**, structuré en modules internes très clairs.

### Architecture cible
Un seul binaire Rust, découpé en modules:

- `market_data`
- `feature_engine`
- `regime_engine`
- `strategy`
- `execution`
- `risk`
- `portfolio`
- `persistence`
- `replay`
- `observability`
- `config`
- `exchange_hyperliquid`

Ce choix permet:
- faible latence
- simplicité opérationnelle
- testabilité correcte
- refactorisation future vers microservices si nécessaire

## 5.3. Langage recommandé pour recherche/backtest: **Python**

Même si le moteur live est en Rust, il est très utile d’avoir un volet recherche en Python pour:
- analyser les historiques exportés
- faire des notebooks
- calibrer les seuils
- générer des rapports
- comparer des variantes de features

### Décision finale
- **Live trading**: Rust
- **Recherche / analyse / calibration / reporting**: Python
- **Stockage de données**: Parquet + DuckDB au départ
- **Évolution possible**: ClickHouse si le volume grossit

---

## 6. Architecture technique détaillée

## 6.1. Vue d’ensemble

```text
Hyperliquid WebSocket / Info API
            |
            v
   [market_data ingestion]
            |
            v
      [state store local]
            |
            +--------------------+
            |                    |
            v                    v
   [feature_engine]       [portfolio/account sync]
            |                    |
            v                    |
      [regime_engine]            |
            |                    |
            v                    |
        [strategy] <-------------+
            |
            v
          [risk]
            |
            v
       [execution]
            |
            v
Hyperliquid Exchange API / WS post
            |
            v
       [fills / acks / rejects]
            |
            +---------> [state reconciliation]
            |
            +---------> [persistence + metrics + alerts]
```

## 6.2. Règle d’or d’architecture

Aucune décision de trading ne doit dépendre d’un état “implicite” dispersé.
Chaque décision doit dépendre d’un état **explicite, sérialisable, rejouable et audit-able**.

En pratique:
- tout événement important est journalisé
- tout ordre possède un `client_order_id`
- toute transition d’état est enregistrée
- toute feature critique peut être recalculée offline

---

## 7. Découpage des modules

## 7.1. Module `exchange_hyperliquid`

Responsabilité:
- encapsuler toutes les spécificités Hyperliquid

Sous-parties:
- signatures
- agent wallet / API wallet
- nonces
- mapping asset id
- ordre limit / ALO / IOC / GTC
- lecture des métadonnées d’assets
- récupération de l’état du compte
- mapping des erreurs Hyperliquid
- logique de reconnect et resync

### Points d’attention
- bien distinguer **wallet signataire** et **adresse du compte/subaccount**
- gérer la rotation éventuelle des API wallets
- prévoir une validation stricte des formats de prix / taille / tick / lot size
- mapper proprement les erreurs de rejet

## 7.2. Module `market_data`

Responsabilité:
- gérer les connexions WebSocket
- reconstruire l’état local nécessaire
- produire un flux d’événements propre pour les modules aval

Flux à écouter:
- order book
- trades
- mids si utile
- user fills / user events
- notifications ordre/annulation si nécessaire

Sorties internes:
- `BookUpdate`
- `TradePrint`
- `MidUpdate`
- `UserFill`
- `OrderAck`
- `OrderReject`
- `ReconnectEvent`
- `SnapshotLoaded`

### Points d’attention
- toute reconnexion doit réhydrater l’état correctement
- les snapshots initiaux doivent être traités de façon idempotente
- attention aux messages reçus hors ordre logique côté code local
- les timestamps locaux et exchange doivent être stockés séparément

## 7.3. Module `state_store`

Responsabilité:
- garder un état mémoire cohérent et ultra-rapide

Contenu:
- meilleur bid/ask
- profondeur N niveaux
- dernières trades
- rolling windows
- ordres ouverts
- queue estimates
- position courante
- marge estimée
- pnl réalisé / latent

### Points d’attention
- éviter les copies inutiles
- séparer état marché et état portefeuille
- définir des invariants stricts
- protéger contre les états partiellement mis à jour

## 7.4. Module `feature_engine`

Responsabilité:
- calculer en temps réel les features microstructurelles

Features principales:
- spread absolu
- spread relatif
- imbalance top 1 / top 3 / top 5 / top 10
- micro-price
- VAMP
- pression d’agression acheteuse/vendeuse
- signed trade flow
- realized volatility courte
- slope du carnet
- depth ratio
- refill speed
- cancel/add ratios
- short term drift
- distance au mid
- time since last micro move
- tick-to-trade ratio local
- impact proxy

### Points d’attention
- toutes les features doivent être **définies mathématiquement**
- éviter les features redondantes
- vérifier la stabilité numérique
- logger les distributions des features
- détecter les features “mortes” ou quasi constantes

## 7.5. Module `regime_engine`

Responsabilité:
- classer le contexte marché avant tout trade

Régimes possibles:
- `QUIET_TIGHT`
- `QUIET_THIN`
- `ACTIVE_HEALTHY`
- `ACTIVE_TOXIC`
- `WIDE_SPREAD`
- `NEWSLIKE_CHAOS`
- `LOW_SIGNAL`
- `DO_NOT_TRADE`

Exemples de critères:
- spread
- profondeur
- volatilité
- taux d’updates
- fill imbalance
- signal-to-noise
- proximité d’une échéance funding
- probabilité de spoofing / toxicité locale

### Pourquoi ce module est crucial
Beaucoup de bots perdent non pas à cause d’un mauvais signal, mais parce qu’ils traitent de la même manière:
- un marché sain
- un marché vide
- un marché chaotique

## 7.6. Module `strategy`

Responsabilité:
- transformer features + régime + état portefeuille en intention de trade

Sorties:
- `NoTrade`
- `PlacePassiveEntry`
- `AmendPassiveEntry`
- `CancelEntry`
- `PlacePassiveExit`
- `ForceExitIOC`
- `ReducePosition`
- `Cooldown`

La stratégie ne doit pas directement “parler exchange”.
Elle émet des **intentions**.
Le module d’exécution décide du “comment”.

## 7.7. Module `risk`

Responsabilité:
- garder le bot vivant

Fonctions:
- sizing
- max position
- max leverage effectif
- stop loss logique
- max loss par trade
- max loss par session
- max daily drawdown
- max concurrent positions
- kill-switch
- cooldown après série de pertes
- blocage durant régime toxique
- blocage proche funding si non désiré

### Règle
Le risque a droit de veto absolu sur la stratégie.

## 7.8. Module `execution`

Responsabilité:
- convertir une intention en ordres Hyperliquid corrects

Sous-fonctions:
- calcul du prix limite
- arrondi tick/lot
- choix ALO / IOC / GTC
- placement
- cancel/replace
- suivi du fill partiel
- timeout d’ordre
- transition maker -> taker si nécessaire
- déduplication des requêtes
- rate limiting local
- gestion des inflight orders

## 7.9. Module `portfolio`

Responsabilité:
- vérité interne du portefeuille de trading

Contenu:
- positions
- qty
- average entry
- pnl réalisé
- pnl latent
- funding cumulé
- frais cumulés
- marge libre estimée
- ordre d’ouverture / fermeture associés

### Important
Le portefeuille interne doit être régulièrement réconcilié avec la vérité exchange.

## 7.10. Module `persistence`

Responsabilité:
- stocker ce qu’il faut pour:
  - audit
  - replay
  - backtest
  - debug
  - mesure de performance

À stocker:
- événements market data utiles
- features calculées
- ordres envoyés
- acks / rejects
- fills
- snapshots périodiques d’état
- métriques agrégées

## 7.11. Module `replay`

Responsabilité:
- rejouer les sessions passées
- reproduire les décisions
- comparer stratégie attendue vs observée

Ce module est fondamental pour débugger un bot court terme.

## 7.12. Module `observability`

Responsabilité:
- métriques
- logs structurés
- dashboards
- alertes
- traces d’événements critiques

## 7.13. Module `config`

Responsabilité:
- centraliser toute la configuration runtime

Format recommandé:
- `TOML` ou `YAML`

Exemples:
- actifs autorisés
- limites de risque
- paramètres de features
- seuils régime
- seuils d’entrée/sortie
- toggles de sécurité
- mode simulation / paper / live

---

## 8. Univers de trading

## 8.1. Principe

Ne **pas** commencer par “plein de cryptos”.

Le bot doit démarrer sur un univers restreint:
- BTC
- ETH
- SOL éventuellement
- 1 à 3 autres actifs seulement après validation

## 8.2. Critères de sélection d’actif

Chaque actif doit être noté selon:
- levier max
- profondeur
- spread médian
- fréquence de mise à jour du carnet
- qualité des fills
- coût moyen d’exécution
- stabilité du tick / lot behavior
- présence de comportements erratiques

## 8.3. Règle V1

Commencer par **1 seul actif live**.
Au maximum **2** pendant la phase de montée en charge.

---

## 9. Données nécessaires

## 9.1. Données temps réel

À capter:
- L2 order book
- trades
- mids
- user fills
- ordre state / rejets / annulations
- funding / mark / oracle si utile pour filtres

## 9.2. Données de référence

À récupérer régulièrement:
- metadata actifs
- tick / lot constraints
- levier max
- margin data
- frais utilisateur
- état du compte
- historique d’ordres et de fills pour réconciliation

## 9.3. Données à persister

### Obligatoires
- book snapshots périodiques
- deltas de book
- trades
- features
- signaux
- décisions
- ordres
- fills
- pnl timeline

### Optionnelles en V1
- raw full depth complète si coût stockage acceptable
- latence fine par étape
- empreintes mémoire/CPU

---

## 10. Définition précise des features

## 10.1. Spread relatif

```text
spread_rel = (best_ask - best_bid) / mid
mid = (best_bid + best_ask) / 2
```

Utilité:
- savoir si le contexte est tradable
- déterminer si une entrée maker a du sens

## 10.2. Order Flow Imbalance (OFI)

Mesure la pression relative du carnet et/ou du flux.

Approches possibles:
- imbalance sur quantités top-of-book
- OFI sur variations successives du carnet
- signed aggressor flow sur fenêtre courte

Le bot devra tester plusieurs variantes:
- OFI_1
- OFI_3
- OFI_5
- OFI_event_window
- signed_trade_flow_1s / 3s / 10s

## 10.3. Micro-price

Prix implicite court terme pondéré par la pression bid/ask.

Utilité:
- détecter une probabilité de déplacement du mid
- comparer micro-price à mid-price

## 10.4. VAMP

Estimateur plus robuste qu’un simple midpoint dans certains contextes.

Utilité:
- filtre directionnel
- estimation d’un “fair micro price”

## 10.5. Depth ratio

Comparer profondeur bid et ask:
- top 1
- top 3
- top 5
- top 10

## 10.6. Book slope

Mesurer comment la liquidité est répartie à mesure qu’on s’éloigne du top-of-book.

Utilité:
- détecter carnet fragile / creux
- estimer le risque d’impact

## 10.7. Refill speed

Après consommation ou annulation d’un niveau:
- à quelle vitesse la liquidité revient-elle ?

Utilité:
- détecter vrai déséquilibre vs simple bruit

## 10.8. Cancel/add ratio

Indicateur de toxicité potentielle:
- trop d’ajouts puis annulations rapides peut signaler un contexte peu sain

## 10.9. Realized volatility courte

Fenêtres:
- 3s
- 10s
- 30s
- 60s

Utilité:
- filtrer:
  - volatilité trop faible => pas assez d’edge
  - volatilité trop élevée => adverse selection trop forte

## 10.10. Trade aggression imbalance

Sur les derniers trades:
- nombre
- volume
- direction
- persistance

Utilité:
- confirmer le biais
- détecter essoufflement

## 10.11. Queue desirability score

Score interne pour estimer si cela vaut la peine de se mettre passivement à un prix donné.

Composants possibles:
- spread
- imbalance
- toxicité
- profondeur devant nous
- probabilité de fill
- probabilité de fill adverse
- temps de présence estimé

---

## 11. Logique détaillée de la stratégie

## 11.1. Pré-filtre de tradabilité

Avant même de parler de direction, le marché doit être **tradable**.

Conditions possibles:
- spread relatif < seuil max
- profondeur top N > minimum
- volatilité dans bande acceptable
- pas de reconnect récent non stabilisé
- pas de drift extrême
- pas de funding boundary proche si filtre activé
- pas de kill-switch actif
- pas de cooldown

Si une seule condition critique échoue:
- `DO_NOT_TRADE`

## 11.2. Signal directionnel

Score continu:

```text
direction_score =
    w1 * normalized_ofi
  + w2 * normalized_microprice_edge
  + w3 * signed_trade_flow
  + w4 * depth_ratio
  + w5 * short_term_return_persistence
  - w6 * toxicity_score
```

Décision:
- `long_bias` si score > seuil_long
- `short_bias` si score < seuil_short
- sinon rien

## 11.3. Condition de pullback

Une fois le biais détecté, ne pas entrer immédiatement.
Attendre un micro-retour:

### Pour un long
- le marché a montré pression acheteuse
- petit retour local vers le bid ou near-bid
- pas de dégradation soudaine du signal
- on tente un ALO buy

### Pour un short
- miroir

## 11.4. Gestion du placement passif

L’entrée passive doit respecter:
- prix aligné tick size
- taille alignée szDecimals
- valeur min > contrainte exchange
- distance au top cohérente
- durée de vie max définie
- annulation si signal se dégrade

## 11.5. Conversion maker -> taker

Il faut une règle explicite.

Un bot “maker-first” ne doit pas devenir un bot “maker-only dogmatique”.

Conditions possibles pour basculer en sortie IOC:
- signal inverse fort
- accélération adverse
- book collapse local
- spread qui s’élargit brutalement
- vol qui explose
- time stop dépassé
- max adverse excursion atteinte

## 11.6. Sorties

Types de sortie:
- take profit passif
- sortie partielle passive
- sortie totale passive
- sortie agressive IOC
- stop logique
- time stop
- exit avant funding
- exit avant régime interdit

## 11.7. Time stop

Un trade 1m/3m qui “ne fait rien” trop longtemps devient suspect.

Exemples:
- pas de progression du trade après X secondes
- remplissage partiel sans follow-through
- trop de temps exposé pour un edge de microstructure

Le time stop est **obligatoire**.

## 11.8. Gestion des fills partiels

Cas fréquents et délicats:
- entrée partielle
- sortie partielle
- annulation partielle
- renversement de signal avec position incomplète

Il faut une state machine claire:
- `ENTRY_PENDING`
- `ENTRY_PARTIAL`
- `ENTRY_FILLED`
- `EXIT_PENDING`
- `EXIT_PARTIAL`
- `EXIT_FILLED`
- `FORCE_EXIT`
- `FLAT`

---

## 12. State machine de trading recommandée

```text
FLAT
  -> SETUP_DETECTED
  -> ENTRY_WORKING
  -> ENTRY_PARTIAL
  -> IN_POSITION
  -> EXIT_WORKING
  -> EXIT_PARTIAL
  -> FLAT

Branches spéciales:
- ENTRY_CANCELLED -> FLAT
- FORCE_EXIT -> FLAT
- ERROR_RECOVERY -> SAFE_MODE
```

### Règle
Aucun module ne doit déduire implicitement l’état à partir de plusieurs drapeaux contradictoires.
Un **enum d’état unique** doit être la vérité.

---

## 13. Gestion du risque

## 13.1. Principes

Pour un bot court terme, la survie dépend plus du risk management que du signal.

## 13.2. Limites obligatoires

### Par trade
- max notional
- max loss
- max slippage toléré
- max hold time

### Par actif
- max position
- max nombre de trades par fenêtre
- max pertes consécutives

### Global
- max daily drawdown
- max intraday drawdown
- max open exposure
- max concurrent positions
- kill-switch global

## 13.3. Levier

Règle de conception:
- ne jamais utiliser le levier max théorique comme cible par défaut
- utiliser un **levier opérationnel** plus bas
- ajuster par actif et régime

Exemple de philosophie:
- actif très liquide + spread propre + vol normale => levier acceptable mais modéré
- actif moins liquide => réduire fortement
- marché toxique => 0 trade

## 13.4. Cross vs isolated

Recommandation V1:
- utiliser **compte dédié / subaccount**
- privilégier une gestion qui borne clairement le risque
- éviter de mélanger des positions diverses en cross sans nécessité

Pour un premier bot:
- **subaccount dédié**
- préférence pour logique de risque de type **isolated par position ou par actif**
- éviter que plusieurs erreurs simultanées contaminent tout le capital

## 13.5. Funding

Hyperliquid paie le funding toutes les heures.

Même si l’horizon est court:
- garder une position juste avant un funding défavorable peut dégrader le PnL
- le bot doit connaître le temps restant avant funding
- prévoir un filtre:
  - pas de nouvelle entrée X minutes/secondes avant funding
  - ou seulement si edge attendu > coût funding
  - ou forcer la sortie avant funding selon actif

## 13.6. Liquidation

Le bot ne doit jamais compter sur “ça passera”.
Les stops logiques et le dimensionnement doivent viser à rendre la liquidation pratiquement impossible en fonctionnement normal.

## 13.7. Kill-switch

Déclenchement si:
- drawdown > seuil
- nombre d’erreurs exchange trop élevé
- désynchronisation état interne / exchange
- reconnects trop fréquents
- taux de rejects anormal
- latence trop haute
- dérive du PnL par rapport au backtest live attendu
- book stream suspect ou incomplet

---

## 14. Exécution détaillée

## 14.1. Principe général

L’exécution est un produit à part entière.
Une bonne stratégie avec mauvaise exécution perdra quand même.

## 14.2. Politique d’entrée

Par défaut:
- ordre limit ALO
- pas d’entrée taker sauf règle spéciale explicitement activée

## 14.3. Politique de sortie

Ordre de préférence:
1. sortie passive
2. sortie passive améliorée
3. sortie IOC défensive
4. liquidation manuelle impossible car la position devrait déjà être coupée

## 14.4. Ordres ALO

Cas à gérer:
- ordre ALO rejeté car croise
- ordre ALO annulé par marché mouvant
- ordre ALO jamais servi
- ordre ALO servi partiellement puis contexte change

## 14.5. Timeout d’ordre

Tout ordre d’entrée doit avoir:
- âge max
- logique de cancel
- logique de repricing limitée
- limite au nombre d’amend/cancel

## 14.6. Client Order ID

Tous les ordres doivent avoir un `cloid` unique, traçable et rejouable.
Convention recommandée:

```text
{strategy}-{asset}-{session}-{seq}-{intent}
```

Exemple:
```text
mfdp-btc-20260331a-000421-entry
```

## 14.7. Rate limiting local

Même si Hyperliquid tolère des débits élevés, le bot doit:
- limiter ses bursts
- regrouper intelligemment
- éviter les boucles cancel/replace pathologiques
- surveiller messages/minute et inflight posts

## 14.8. Réconciliation d’ordre

Après envoi d’un ordre:
- attendre ack / reject / fill / cancel
- mettre à jour l’état local
- recoller périodiquement avec ordre status / historique
- déclencher alerte si divergence

---

## 15. Points d’attention Hyperliquid spécifiques

## 15.1. API wallets / agent wallets

Le bot doit utiliser un schéma propre:
- wallet maître sécurisé
- agent/API wallet pour signer
- subaccount dédié au bot si possible

Piège important:
- l’API wallet sert à signer
- pour interroger les données du compte il faut utiliser l’adresse du vrai compte/subaccount

## 15.2. Signatures

Ne pas implémenter les signatures “à la main” sans raison sérieuse.
Utiliser une implémentation éprouvée ou reproduite très fidèlement avec tests croisés.

## 15.3. Tick / lot size

Tout ordre doit être:
- arrondi correctement
- validé avant envoi
- testé offline sur chaque actif autorisé

## 15.4. Minimum trade size / notional

Le sizing doit respecter la contrainte d’ordre minimum.
Le moteur de sizing ne doit jamais générer des ordres invalides.

## 15.5. WebSocket reconnect

Hyperliquid peut déconnecter périodiquement.
Le bot doit:
- reconnecter proprement
- recharger snapshot
- rattraper l’état
- suspendre le trading tant que l’état n’est pas sain

## 15.6. Batch behavior

Les batchs doivent être utilisés intelligemment mais sans rendre le debug impossible.

## 15.7. TP/SL

Les TP/SL Hyperliquid sont déclenchés selon leur logique de trigger et sont automatiquement des market orders.
Pour une stratégie ultra-court terme, il est préférable de garder la **logique de sortie principale dans le moteur**, pas de déléguer aveuglément au seul exchange.

## 15.8. Margin mode / account abstraction

Il faut figer une convention d’account mode dès le départ et la documenter pour éviter les surprises opérationnelles.

---

## 16. Modèle de décision détaillé

## 16.1. Séquence complète d’un trade

1. réception d’updates carnet/trades
2. mise à jour de l’état local
3. recalcul des features
4. classification du régime
5. évaluation du signal directionnel
6. vérification du pré-filtre risque
7. décision d’entrée passive
8. placement ordre ALO
9. suivi fill / partial fill / cancel
10. passage en position
11. suivi continu du trade
12. sortie passive ou agressive selon contexte
13. enregistrement résultat
14. cooldown éventuel

## 16.2. Priorité des décisions

Ordre de priorité:
1. sécurité système
2. cohérence de l’état
3. gestion du risque
4. gestion de position
5. nouvelles entrées
6. optimisation du prix

---

## 17. Backtesting et simulation

## 17.1. Règle absolue

Pas de mise en prod sans:
- replay
- backtest événementiel
- simulation des fills
- estimation réaliste des frais
- pénalisation du slippage
- pénalisation de la latence
- modélisation des fills partiels

## 17.2. Ce qu’il ne faut pas faire

Ne surtout pas backtester uniquement sur chandeliers OHLCV.
Pour cette stratégie, ce serait trompeur.

## 17.3. Niveau minimal de backtest

Le backtest doit être:
- event-driven
- basé sur book + trades + règles de fill plausibles
- capable de simuler:
  - entrée maker non servie
  - entrée maker servie partiellement
  - adverse selection
  - sortie IOC
  - funding
  - frais réels
  - délais de décision

## 17.4. Modèle de fill recommandé V1

Pas besoin d’un modèle parfait, mais il faut un modèle honnête.

Approche V1:
- si ordre ALO placé à un prix qui devient top-of-book
- estimer une file d’attente simplifiée
- ne considérer le fill que si un volume suffisant passe
- appliquer une pénalité conservative
- simuler moins de fills que “l’optimisme naturel”

## 17.5. Pénalités à injecter

Le backtest doit inclure:
- frais maker/taker
- slippage
- latence décision
- latence envoi
- fill incomplet
- cancels/repricing inefficaces
- pertes supplémentaires sur sorties défensives

## 17.6. Métriques de validation

À suivre:
- expectancy nette par trade
- hit rate
- avg winner / avg loser
- max adverse excursion
- max favorable excursion
- temps moyen en position
- part maker / taker
- pnl net par régime
- pnl net par actif
- pnl net par heure de la journée
- turnover
- coût total en frais
- coût total en slippage
- rate de fills partiels
- ratio entrées annulées / servies
- drawdown max
- stabilité des résultats hors échantillon

---

## 18. Pipeline de recherche

## 18.1. Étape 1 — Captation de données

D’abord collecter plusieurs jours/semaines de données:
- book
- trades
- fills
- features brutes

## 18.2. Étape 2 — Analyse exploratoire

Objectifs:
- distribution des spreads
- régimes de volatilité
- qualité de profondeur
- comportement par actif
- moments de toxicité
- fréquence de fills potentielles

## 18.3. Étape 3 — Étude de prédictibilité

Tester:
- OFI vs rendement futur 1s / 3s / 10s / 30s
- micro-price edge vs direction future
- VAMP edge
- signaux combinés
- robustesse selon régime

## 18.4. Étape 4 — Construction des règles

D’abord règles simples, lisibles, auditables.
Éviter de commencer par un modèle ML opaque.

## 18.5. Étape 5 — Validation hors échantillon

Séparer:
- calibration
- validation
- forward testing

## 18.6. Étape 6 — Replay paper trading

Rejouer session par session.
Comparer:
- ce que le bot aurait dû faire
- ce qu’il a fait
- ce qu’il a effectivement obtenu

---

## 19. Ordre de développement recommandé

## 19.1. Phase 0 — Design

Livrables:
- spec d’architecture
- dictionnaire d’événements
- définitions des features
- state machine
- politique de risque
- mapping des erreurs exchange

## 19.2. Phase 1 — Connectivité Hyperliquid

Objectifs:
- connexion WebSocket stable
- récupération metadata
- lecture état compte
- envoi ordre testnet / très petit live
- gestion signatures
- gestion API wallet
- reconnexion propre

Critère de sortie:
- plus de désynchronisation simple
- ordres de test fiables
- logs et métriques de base disponibles

## 19.3. Phase 2 — State store + persistence

Objectifs:
- carnet local cohérent
- stockage brut
- modèle événementiel stable
- snapshots réguliers

Critère de sortie:
- possibilité de rejouer une session sans trous majeurs

## 19.4. Phase 3 — Features + dashboards

Objectifs:
- calcul streaming
- monitoring distributions
- graphes diagnostics

Critère de sortie:
- features plausibles, stables, interprétables

## 19.5. Phase 4 — Moteur de replay

Objectifs:
- rejouer décisions
- injecter règles
- mesurer PnL simulé

Critère de sortie:
- backtests reproductibles

## 19.6. Phase 5 — Stratégie V1 en paper trading

Objectifs:
- aucun ordre live
- juste décisions et exécution simulée
- comparaison en temps réel avec le marché

Critère de sortie:
- stabilité sur plusieurs jours
- pas de comportement aberrant

## 19.7. Phase 6 — Live pilot ultra-réduit

Objectifs:
- taille minimale
- un seul actif
- levier très bas
- horaires limités
- surveillance renforcée

Critère de sortie:
- comportement stable
- pas de divergence état / exchange
- drawdown acceptable
- coûts conformes aux attentes

## 19.8. Phase 7 — Amélioration incrémentale

Ajouter:
- meilleurs filtres de régime
- meilleur modèle de fill
- meilleure logique de repricing
- second actif
- optimisation des seuils
- recherche de variantes

---

## 20. Stockage et observabilité

## 20.1. Stockage recommandé V1

- événements bruts en fichiers journaliers Parquet
- indexation/lecture avec DuckDB
- snapshots compacts pour redémarrage rapide

### Pourquoi ce choix
- simple
- peu coûteux
- très pratique pour analyse offline
- assez robuste pour un démarrage

## 20.2. Évolution possible

Quand le volume augmente:
- ClickHouse pour requêtes temps réel et dashboards avancés

## 20.3. Logs

Logs structurés JSON:
- niveau
- horodatage local
- horodatage exchange
- asset
- state
- signal
- décision
- order id
- cloid
- latence
- erreur éventuelle

## 20.4. Métriques à exporter

- ws reconnect count
- snapshot reload count
- order rejects
- order cancels
- passive fill rate
- maker share
- taker share
- pnl brut
- pnl net
- fees cumulated
- funding cumulated
- slippage estimated
- average queue lifetime
- trade count
- kill-switch count
- state divergence count

## 20.5. Alertes

Alertes temps réel si:
- plus de N reconnects / heure
- divergence portefeuille
- drawdown seuil
- ordre rejeté répété
- latence anormale
- absence de data
- absence de fill trop longue vs comportement attendu
- kill-switch actif

---

## 21. Sécurité opérationnelle

## 21.1. Wallets

- wallet maître séparé
- subaccount dédié bot
- agent/API wallet dédié à ce bot
- permissions minimales
- procédure de rotation documentée

## 21.2. Secrets

- jamais en dur dans le code
- injection par variables d’environnement ou secret store
- chiffrement au repos selon environnement

## 21.3. Isolation

- machine dédiée au bot ou au moins runtime isolé
- pas d’autres workloads lourds sur la même machine

## 21.4. Audit trail

Tout événement important doit pouvoir être reconstitué:
- ordre envoyé
- signature
- réponse
- fill
- décision de sortie
- kill-switch

---

## 22. Infrastructure recommandée

## 22.1. Environnement de développement

- Rust stable
- Tokio
- serde
- tracing
- metrics/prometheus
- parquet/arrow
- DuckDB côté analyse

## 22.2. Environnement de production V1

- VPS Linux dédié
- faible jitter réseau
- monitoring système
- process manager type systemd
- restart policy maîtrisée
- disque suffisant pour journaux et données

## 22.3. Évolution latence

Si le projet devient sérieux:
- machine plus performante
- optimisation réseau
- éventuellement non-validating node pour réduire la latence de marché/état
- profiling CPU/mémoire

---

## 23. Choix de librairies / briques techniques recommandées

## 23.1. Rust live engine

Briques probables:
- `tokio` pour async runtime
- `tokio-tungstenite` ou équivalent WebSocket
- `reqwest` pour HTTP
- `serde` / `serde_json`
- `rust_decimal` ou représentation fixed-point stricte
- `thiserror` / `anyhow`
- `tracing`
- `uuid` ou générateur maison pour IDs
- `parquet` / `arrow`
- `prometheus` client
- `clap` pour CLI éventuelle
- `config` ou parsing TOML/YAML
- `dashmap` seulement si besoin, sinon préférer ownership claire et canaux

## 23.2. Python research

- pandas/polars
- numpy
- scipy
- scikit-learn pour études simples
- jupyter
- duckdb
- matplotlib/plotly selon préférence

---

## 24. Modélisation numérique

## 24.1. Règle

Éviter les flottants “naïfs” pour la logique critique d’ordre.

Utiliser:
- décimaux exacts ou fixed-point
- fonctions d’arrondi centralisées

## 24.2. Fonctions obligatoires

- `round_price_to_tick(asset, px)`
- `round_size_to_lot(asset, sz)`
- `min_valid_order_size(asset, px)`
- `is_order_valid(asset, px, sz)`

Ces fonctions doivent être:
- testées
- déterministes
- réutilisées partout

---

## 25. Tests à écrire

## 25.1. Tests unitaires

- arrondis
- conversions
- signaux
- transitions d’état
- risk veto
- calcul PnL
- calcul funding
- time stop

## 25.2. Tests d’intégration

- WebSocket connect/reconnect
- parsing messages
- snapshot + deltas
- placement / cancel ordre
- fill reconciliation
- API wallet handling

## 25.3. Tests de simulation

- partial fills
- rejet tick size
- rejet min notional
- perte de connexion
- double message
- message tardif
- divergence ordre/fill
- bascule force exit

## 25.4. Golden tests / replay tests

Conserver des sessions réelles et vérifier que:
- les mêmes inputs donnent les mêmes décisions
- toute régression est détectée

---

## 26. Points d’attention métier / stratégie

## 26.1. Ne pas sur-optimiser

Le danger n°1 sera probablement l’overfitting:
- seuils trop précis
- trop de filtres
- calibration sur un seul actif
- calibration sur une seule période

## 26.2. Attention aux heures de marché

Même en crypto, la microstructure change selon les heures.
Mesurer:
- heures fortes
- heures creuses
- périodes de funding
- périodes d’annonces macro
- périodes weekend/weekday

## 26.3. Attention aux actifs “jolis en backtest”

Un actif peut sembler rentable:
- parce qu’on surestime les fills
- parce qu’on ignore la toxicité
- parce que le spread moyen trompe sur la queue des situations

## 26.4. Attention aux fake edges

Un edge apparent peut disparaître après ajout de:
- frais
- slippage
- latence
- cancels non servis
- fills partiels
- funding

## 26.5. Attention à l’adverse selection

C’est le problème central d’un maker-first bot:
- être exécuté précisément quand on n’aurait pas dû

Donc:
- filtrer la toxicité
- limiter le temps d’exposition passive
- annuler vite si le signal change
- ne pas “espérer” un rebond

---

## 27. Paramètres de départ recommandés

Ces valeurs sont indicatives, pas des valeurs finales.

## 27.1. Paramètres univers
- 1 actif live au départ
- levier faible à modéré
- 1 seule position ouverte à la fois
- pas de nouvelle entrée en cas de reconnect récent
- cooldown après perte

## 27.2. Paramètres entrée
- spread max strict
- profondeur min stricte
- signal score min assez élevé
- pullback obligatoire
- ALO obligatoire

## 27.3. Paramètres sortie
- time stop court
- sortie agressive si retournement fort
- pas de stubborn holding
- sortie avant funding selon configuration

## 27.4. Paramètres risque
- loss max par trade très borné
- daily stop
- kill-switch sur erreurs techniques

---

## 28. KPI de réussite du projet

Le bot sera considéré comme bien conçu si:

### Techniquement
- il tourne plusieurs jours sans divergence d’état
- il reconnecte proprement
- il journalise tout
- il passe les tests de replay
- il garde la cohérence portefeuille / exchange

### Stratégiquement
- son expectancy nette reste positive ou proche de positive sur paper/live pilote
- sa part maker est élevée sur les entrées
- ses coûts réels restent proches des hypothèses
- ses pertes en régime toxique sont coupées vite
- il ne dépend pas d’un seul pattern rare

### Opérationnellement
- il peut être arrêté / redémarré sans corruption
- il a des dashboards lisibles
- il possède des garde-fous clairs

---

## 29. Ce qu’il faudra documenter en parallèle

Produire dès le début:
- un `README.md` racine
- une `ARCHITECTURE.md`
- une `STRATEGY.md`
- une `RISK_POLICY.md`
- une `RUNBOOK.md`
- une `REPLAY_GUIDE.md`
- une `CONFIG_REFERENCE.md`
- une `POSTMORTEM_TEMPLATE.md`

---

## 30. Structure de repository recommandée

```text
hyperliquid-mfdp-bot/
├─ README.md
├─ docs/
│  ├─ ARCHITECTURE.md
│  ├─ STRATEGY.md
│  ├─ RISK_POLICY.md
│  ├─ RUNBOOK.md
│  ├─ REPLAY_GUIDE.md
│  └─ CONFIG_REFERENCE.md
├─ config/
│  ├─ default.toml
│  ├─ paper.toml
│  └─ live.toml
├─ crates/
│  ├─ app/
│  ├─ exchange_hyperliquid/
│  ├─ market_data/
│  ├─ state_store/
│  ├─ feature_engine/
│  ├─ regime_engine/
│  ├─ strategy/
│  ├─ risk/
│  ├─ execution/
│  ├─ portfolio/
│  ├─ persistence/
│  ├─ replay/
│  └─ observability/
├─ python-research/
│  ├─ notebooks/
│  ├─ scripts/
│  └─ reports/
├─ tests/
│  ├─ integration/
│  ├─ replay/
│  └─ fixtures/
└─ data/
   ├─ raw/
   ├─ features/
   └─ snapshots/
```

---

## 31. Roadmap réaliste

## Semaine 1-2
- design détaillé
- repo
- config
- connexion WS/HTTP
- metadata actifs
- auth/signature de base

## Semaine 3-4
- state store
- persistence
- user events
- order test flow
- premières métriques

## Semaine 5-6
- features
- dashboards
- collecte données
- premières analyses offline

## Semaine 7-8
- replay engine
- stratégie rules-based V1
- simulation réaliste de fills

## Semaine 9-10
- paper trading
- calibration
- kill-switches
- runbook

## Semaine 11-12
- live pilote ultra-réduit
- revue post mortem de chaque incident
- ajustements

---

## 32. Checklist “prêt pour live”

Avant le vrai live:
- [ ] subaccount dédié créé
- [ ] API wallet dédié et documenté
- [ ] secrets sécurisés
- [ ] tests arrondis / taille OK
- [ ] gestion reconnect validée
- [ ] réconciliation portefeuille validée
- [ ] moteur replay opérationnel
- [ ] paper trading concluant
- [ ] kill-switch testé
- [ ] alertes opérationnelles actives
- [ ] sizing ultra réduit
- [ ] runbook incident écrit
- [ ] procédure arrêt d’urgence écrite

---

## 33. Les plus gros risques du projet

## 33.1. Risques techniques
- mauvaise reconstruction d’état du carnet
- bug de signatures/nonces
- gestion incomplète des reconnects
- divergence portefeuille
- erreurs d’arrondi
- modèle de fill trop optimiste

## 33.2. Risques stratégiques
- edge inexistant après coûts
- edge non stable hors échantillon
- adverse selection trop forte
- marché plus toxique que prévu
- levier insuffisant pour rentabiliser le turnover

## 33.3. Risques opérationnels
- surveillance insuffisante
- trop d’actifs trop tôt
- trop de levier trop tôt
- passage live avant paper trading sérieux
- dépendance à un environnement non maîtrisé

---

## 34. Recommandation finale

Le meilleur plan n’est pas de coder “vite un bot”.
Le meilleur plan est de construire **une plateforme de trading courte durée disciplinée**, dont la première stratégie est ce **bot directionnel maker-first basé sur la microstructure**.

### Décision finale recommandée
- **Stratégie**: directional pullback maker-first, queue-aware, microstructure-driven
- **Langage live**: Rust
- **Langage recherche**: Python
- **Architecture**: modular monolith event-driven
- **Stockage V1**: Parquet + DuckDB
- **Scope V1**: 1 actif, 1 stratégie, 1 position à la fois, risk management très strict

### Pourquoi c’est le meilleur choix
Parce que ce choix:
- respecte la structure de coûts d’Hyperliquid
- tient compte des leviers limités selon les actifs
- réduit la dépendance au taker
- évite la complexité inutile
- reste testable, maintenable et extensible

---

## 35. Sources de référence

### Documentation officielle Hyperliquid
- Fees: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees
- Order types: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/order-types
- Market making: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/market-making
- Margining: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining
- Liquidations: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations
- Funding: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding
- Perpetual assets: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/perpetual-assets
- Contract specifications: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/contract-specifications
- WebSocket: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket
- WebSocket subscriptions: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions
- Exchange endpoint: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint
- Tick and lot size: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size
- Rate limits and user limits: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits
- Nonces and API wallets: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets
- Signing: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/signing
- Optimizing latency: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/optimizing-latency

### Références de recherche utiles
- Explainable Patterns in Cryptocurrency Microstructure: https://arxiv.org/html/2602.00776v1
- Learning to Predict Short-Term Volatility with Order Flow Image Representation: https://arxiv.org/html/2304.02472v2
- Exploring Microstructural Dynamics in Cryptocurrency Limit Order Books: https://arxiv.org/html/2506.05764v2
- RL-Exec: Impact-Aware Reinforcement Learning for Optimal Liquidation: https://arxiv.org/html/2511.07434v1
- OFI modeling overview: https://arxiv.org/pdf/2411.08382
- Hawkes-based cryptocurrency forecasting via LOB data: https://arxiv.org/html/2312.16190v1
