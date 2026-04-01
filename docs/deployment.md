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

### 2.1. Connexion SSH

```bash
ssh gbot
# ou explicitement :
ssh root@<IP_HETZNER>
```

### 2.2. Première installation

```bash
# Sur le serveur gbot
mkdir -p /opt/gbot && cd /opt/gbot

# Cloner le repo (ou scp depuis local)
git clone <repo_url> .
# ou depuis local :
# scp -r ./ gbot:/opt/gbot/

# Builder Docker
docker build -t gbot .

# Créer le fichier .env avec les secrets
cat > /opt/gbot/.env << 'EOF'
GBOT__EXCHANGE__WALLET_ADDRESS=0xYOUR_WALLET
GBOT__EXCHANGE__AGENT_PRIVATE_KEY=YOUR_PRIVATE_KEY
GBOT__GENERAL__MODE=live
RUST_LOG=info
EOF
chmod 600 /opt/gbot/.env
```

### 2.3. Lancer le bot

```bash
cd /opt/gbot

docker run -d \
  --name gbot \
  --restart unless-stopped \
  -p 127.0.0.1:3000:3000 \
  -v /opt/gbot/data:/app/data \
  --env-file /opt/gbot/.env \
  gbot
```

> **Note** : le port est bindé sur `127.0.0.1` seulement — l'UI n'est pas exposée publiquement.

### 2.4. Mettre à jour le bot

```bash
ssh gbot
cd /opt/gbot
git pull
docker build -t gbot .
docker stop gbot && docker rm gbot

# Relancer (même commande que 2.3)
docker run -d \
  --name gbot \
  --restart unless-stopped \
  -p 127.0.0.1:3000:3000 \
  -v /opt/gbot/data:/app/data \
  --env-file /opt/gbot/.env \
  gbot
```

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
docker stop gbot
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
