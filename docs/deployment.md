# gbot Deployment Guide

## Prerequisites

- Rust 1.77+ (local) or Docker
- Hyperliquid API wallet (agent wallet) configured
- Serveur Hetzner `gbot` accessible via SSH (pour le déploiement distant)

## Environment Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `GBOT__EXCHANGE__WALLET_ADDRESS` | Hyperliquid wallet address | Yes (live) |
| `GBOT__EXCHANGE__AGENT_PRIVATE_KEY` | Agent wallet private key (hex) | Yes (live) |
| `GBOT__GENERAL__MODE` | `observation`, `dry-run`, or `live` | No (default: observation) |
| `RUST_LOG` | Log level (`info`, `debug`, `warn`) | No (default: info) |

> **Secrets** : ne jamais commiter de clé privée. Utiliser des variables d'environnement ou un fichier `.env` (ajouté dans `.gitignore`).

---

## 0. Configuration SSH (une seule fois)

Avant tout déploiement, configurer l'accès SSH au serveur Hetzner :

```bash
# 1. Générer une clé dédiée (si pas déjà fait)
ssh-keygen -t ed25519 -C "gbot" -f ~/.ssh/gbot

# 2. Copier la clé sur le serveur (mot de passe root demandé une seule fois)
ssh-copy-id -i ~/.ssh/gbot.pub root@46.224.43.198

# 3. Ajouter l'alias SSH dans ~/.ssh/config
cat >> ~/.ssh/config << 'EOF'

Host gbot
    HostName 46.224.43.198
    User root
    IdentityFile ~/.ssh/gbot
EOF

# 4. Tester
ssh gbot echo "ok"
```

---

## 1. Déploiement local

### Cargo (développement)

```bash
# Observation mode (pas de clé requise)
cargo run

# Dry-run (simule les ordres)
GBOT__GENERAL__MODE=dry-run cargo run

# Live (nécessite wallet + clé)
GBOT__EXCHANGE__WALLET_ADDRESS=0x... \
GBOT__EXCHANGE__AGENT_PRIVATE_KEY=abc123... \
GBOT__GENERAL__MODE=live \
cargo run --release
```

### Docker (local)

```bash
docker build -t gbot .
docker run -d \
  --name gbot \
  -p 3000:3000 \
  -v $(pwd)/data:/app/data \
  -e GBOT__GENERAL__MODE=observation \
  gbot
```

### Accéder à l'UI locale

L'UI est servie par Axum sur le port **3000** :

| URL | Description |
|-----|-------------|
| `http://localhost:3000` | Dashboard principal (single page) |
| `http://localhost:3000/api/state` | Snapshot JSON complet |
| `http://localhost:3000/api/stream` | SSE temps réel (500ms) |
| `http://localhost:3000/health` | Health check |
| `http://localhost:3000/metrics` | Prometheus metrics |

L'UI est **read-only**, sans authentification — ne pas exposer publiquement sans tunnel SSH.

---

## 2. Déploiement sur le serveur Hetzner (`gbot`)

### 2.1. Préparer le serveur (une seule fois)

Le script `prepareServer.sh` installe Docker, fail2ban, ufw, crée l'utilisateur `gbot-deploy`, et prépare `/opt/gbot` :

```bash
./prepareServer.sh 46.224.43.198
```

Ce qu'il fait :
- Installe Docker, fail2ban, ufw
- Configure le firewall (SSH uniquement — port 3000 **non** exposé)
- Crée l'utilisateur `gbot-deploy` avec accès Docker
- Prépare `/opt/gbot/data`
- Sécurise SSH (clé uniquement, pas de mot de passe)
- Augmente les limites de fichiers ouverts (WebSocket + Parquet)
- Active les mises à jour de sécurité automatiques

### 2.2. Configurer les secrets (une seule fois)

```bash
ssh gbot
cat > /opt/gbot/.env << 'EOF'
GBOT__EXCHANGE__WALLET_ADDRESS=0xYOUR_WALLET
GBOT__EXCHANGE__AGENT_PRIVATE_KEY=YOUR_PRIVATE_KEY
GBOT__GENERAL__MODE=observation
RUST_LOG=info
EOF
chmod 600 /opt/gbot/.env
```

### 2.3. Déployer et lancer avec `deploy.sh`

```bash
# Déployer le code + build Docker (sans démarrer)
./deploy.sh

# Déployer + (re)démarrer le bot
./deploy.sh --start
```

Le script :
1. Vérifie les prérequis locaux (Cargo.toml, Dockerfile, static/, connexion SSH)
2. `rsync` le code vers `/opt/gbot` (exclut .git, target, data, .env, research, *.parquet)
3. Build l'image Docker sur le serveur
4. Vérifie la présence du `.env`
5. Avec `--start` : arrête l'ancien container s'il tourne, lance le nouveau, health check

### 2.4. Mettre à jour le bot

Même commande que le déploiement initial :

```bash
./deploy.sh --start
```

Le script arrête automatiquement l'ancien container, rebuild l'image et relance.

### 2.5. Accéder à l'UI depuis ta machine locale

L'UI écoute sur `127.0.0.1:3000` sur le serveur. Pour y accéder depuis ton navigateur local, utilise un **tunnel SSH** :

```bash
# Depuis ta machine locale
ssh -L 3000:127.0.0.1:3000 gbot
```

Puis ouvrir `http://localhost:3000` dans le navigateur.

Pour un tunnel persistant en arrière-plan :

```bash
ssh -f -N -L 3000:127.0.0.1:3000 gbot
```

> **Alternative** : si le port 3000 est déjà pris localement, mapper sur un autre port :
> ```bash
> ssh -L 8080:127.0.0.1:3000 gbot
> # → ouvrir http://localhost:8080
> ```

---

## 3. Commandes utiles

### Logs

```bash
# Logs en temps réel
docker logs -f gbot

# Dernières 100 lignes
docker logs --tail 100 gbot
```

### Statut

```bash
# Depuis le serveur
curl http://127.0.0.1:3000/health
curl http://127.0.0.1:3000/api/state | python3 -m json.tool

# Depuis ta machine (via tunnel SSH)
curl http://localhost:3000/api/state | python3 -m json.tool
```

### Arrêt d'urgence

```bash
ssh gbot 'docker stop gbot'
```

### Déploiement rapide

```bash
# Depuis ta machine locale — tout en une commande
./deploy.sh --start
```

### Récupérer les données pour analyse locale

Le script `fetch-data.sh` télécharge les données du serveur dans `./server-data/` :

```bash
# Dernières 24h (défaut)
./fetch-data.sh

# 3 derniers jours
./fetch-data.sh --days 3

# Date précise
./fetch-data.sh --date 2026-04-01

# Tout
./fetch-data.sh --all

# Uniquement les logs Docker
./fetch-data.sh --logs-only

# Voir ce qui serait téléchargé sans rien faire
./fetch-data.sh --dry-run
```

Le script récupère : l2, trades, features, signaux, ordres, fills, P&L, journal, logs Docker, et un snapshot de l'API `/api/state`.

Analyse ensuite avec DuckDB ou Python/Polars :
```bash
duckdb -c "SELECT * FROM 'server-data/fills/*.parquet' LIMIT 20"
```

### Données

Les données sont persistées dans `/opt/gbot/data/` (volume Docker) :

```
data/
├── l2/{coin}/{date}.parquet     — Book snapshots
├── trades/{coin}/{date}.parquet — Trade tape
├── features/{coin}/{date}.parquet — Computed features
├── signals/{date}.parquet       — Generated signals
├── orders/{date}.parquet        — Placed orders
├── fills/{date}.parquet         — Executed fills
├── pnl/{date}.parquet           — P&L timeline
└── journal/journal_{ts}.jsonl   — Order journal (JSONL debug)
```

---

## 4. Backtest

```bash
# Run backtest on recorded data
cargo run --release -- --backtest --date 2024-11-15 --coins BTC,ETH

# Convert JSONL to Parquet (offline analysis)
cargo run --release -- --convert-parquet --coin BTC --date 2024-11-15
```

---

## 5. Sécurité

- **Jamais** exposer le port 3000 publiquement — toujours `127.0.0.1` + tunnel SSH
- Les secrets sont dans `/opt/gbot/.env` avec permissions `600`
- L'UI est **read-only** (pas de boutons d'action, pas de config runtime)
- Utiliser un subaccount Hyperliquid dédié au bot
- Rotation régulière de l'agent wallet
