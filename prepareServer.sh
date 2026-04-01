#!/usr/bin/env bash
set -euo pipefail
# =============================================================================
# gbot — Préparation du serveur distant
# =============================================================================
# Configure un serveur vierge (Ubuntu 22/24) pour accueillir gbot :
#   - Installe Docker, fail2ban, ufw
#   - Crée l'utilisateur gbot-deploy
#   - Sécurise SSH (clé uniquement)
#   - Prépare le répertoire /opt/gbot avec les bons droits
#   - Configure les mises à jour automatiques de sécurité
#
# S'exécute DEPUIS votre machine locale, nécessite un accès SSH root.
#
# Usage :
#   ./prepareServer.sh <IP_DU_SERVEUR>
# =============================================================================

# ---- Couleurs ----
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ---- Arguments ----
if [ $# -lt 1 ]; then
    echo "Usage: $0 <IP_DU_SERVEUR>"
    echo ""
    echo "  Configure le serveur pour accueillir gbot."
    echo "  Nécessite un accès SSH root (clé SSH configurée sur Hetzner)."
    exit 1
fi

SERVER_IP="$1"
DEPLOY_USER="gbot-deploy"
APP_DIR="/opt/gbot"

ssh_root() { ssh -i ~/.ssh/gbot -o StrictHostKeyChecking=accept-new "root@${SERVER_IP}" "$@"; }

# ---- Main ----
echo ""
echo "========================================="
echo "  gbot — Préparation du serveur"
echo "  Serveur : ${SERVER_IP}"
echo "========================================="
echo ""

info "Configuration du serveur ${SERVER_IP}..."

ssh_root bash <<'SETUP_SCRIPT'
set -euo pipefail

echo ">>> Mise à jour des paquets..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get upgrade -y -qq

# ---- Docker ----
echo ">>> Installation de Docker..."
if ! command -v docker &>/dev/null; then
    apt-get install -y -qq ca-certificates curl gnupg
    install -m 0755 -d /etc/apt/keyrings
    curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
    chmod a+r /etc/apt/keyrings/docker.gpg
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
      https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
      > /etc/apt/sources.list.d/docker.list
    apt-get update -qq
    apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin
    systemctl enable --now docker
    echo "  Docker installé."
else
    echo "  Docker déjà installé."
fi

# ---- Firewall + fail2ban ----
echo ">>> Installation de fail2ban + ufw..."
apt-get install -y -qq fail2ban ufw

echo ">>> Configuration du firewall..."
ufw default deny incoming
ufw default allow outgoing
ufw allow ssh
# Port 3000 accessible UNIQUEMENT en local (tunnel SSH) — pas ouvert dans ufw
ufw --force enable
echo "  Firewall configuré (SSH uniquement — UI via tunnel SSH)."

echo ">>> Activation de fail2ban..."
systemctl enable --now fail2ban

# ---- Utilisateur déploiement ----
echo ">>> Création de l'utilisateur gbot-deploy..."
if ! id gbot-deploy &>/dev/null; then
    useradd -m -s /bin/bash gbot-deploy
    usermod -aG docker gbot-deploy
    mkdir -p /home/gbot-deploy/.ssh
    cp /root/.ssh/authorized_keys /home/gbot-deploy/.ssh/authorized_keys
    chown -R gbot-deploy:gbot-deploy /home/gbot-deploy/.ssh
    chmod 700 /home/gbot-deploy/.ssh
    chmod 600 /home/gbot-deploy/.ssh/authorized_keys
    echo "  Utilisateur gbot-deploy créé."
else
    echo "  Utilisateur gbot-deploy existe déjà."
    # S'assurer qu'il est dans le groupe docker
    usermod -aG docker gbot-deploy 2>/dev/null || true
fi

# ---- Répertoire application ----
echo ">>> Préparation de /opt/gbot..."
mkdir -p /opt/gbot/data
chown -R gbot-deploy:gbot-deploy /opt/gbot
echo "  /opt/gbot prêt."

# ---- Sécurisation SSH ----
echo ">>> Sécurisation SSH..."
sed -i 's/^#\?PermitRootLogin .*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config
sed -i 's/^#\?PasswordAuthentication .*/PasswordAuthentication no/' /etc/ssh/sshd_config
systemctl reload ssh 2>/dev/null || systemctl reload sshd 2>/dev/null || true

# ---- Paramètres système ----
echo ">>> Configuration des limites système..."
echo 'vm.overcommit_memory=1' > /etc/sysctl.d/99-gbot.conf
# Augmenter les limites de fichiers ouverts (utile pour WebSocket + Parquet)
echo 'fs.file-max=1000000' >> /etc/sysctl.d/99-gbot.conf
sysctl -p /etc/sysctl.d/99-gbot.conf 2>/dev/null || true

# Limites par processus pour l'utilisateur gbot-deploy
cat > /etc/security/limits.d/99-gbot.conf <<'LIMITS'
gbot-deploy soft nofile 65536
gbot-deploy hard nofile 65536
LIMITS

# ---- Mises à jour auto de sécurité ----
echo ">>> Configuration des mises à jour automatiques de sécurité..."
apt-get install -y -qq unattended-upgrades
echo 'Unattended-Upgrade::Automatic-Reboot "false";' > /etc/apt/apt.conf.d/51custom-unattended

echo ""
echo ">>> Setup serveur terminé !"
SETUP_SCRIPT

ok "Serveur configuré avec succès"
echo ""
echo "  Prochaines étapes :"
echo ""
echo "  1. Copier le code sur le serveur :"
echo "     scp -r . gbot-deploy@${SERVER_IP}:/opt/gbot/"
echo ""
echo "  2. Se connecter et builder :"
echo "     ssh gbot-deploy@${SERVER_IP}"
echo "     cd /opt/gbot && docker build -t gbot ."
echo ""
echo "  3. Créer le fichier .env :"
echo "     cat > /opt/gbot/.env << 'EOF'"
echo "     GBOT__EXCHANGE__WALLET_ADDRESS=0xYOUR_WALLET"
echo "     GBOT__EXCHANGE__AGENT_PRIVATE_KEY=YOUR_KEY"
echo "     GBOT__GENERAL__MODE=live"
echo "     RUST_LOG=debug"
echo "     EOF"
echo "     chmod 600 /opt/gbot/.env"
echo ""
echo "  4. Lancer le bot :"
echo "     docker run -d --name gbot --restart unless-stopped \\"
echo "       -p 127.0.0.1:3000:3000 \\"
echo "       -v /opt/gbot/data:/app/data \\"
echo "       --env-file /opt/gbot/.env gbot"
echo ""
echo "  5. Accéder à l'UI depuis votre machine :"
echo "     ssh -L 3000:127.0.0.1:3000 gbot-deploy@${SERVER_IP}"
echo "     → http://localhost:3000"
echo ""
