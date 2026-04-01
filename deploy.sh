#!/usr/bin/env bash
set -euo pipefail
# =============================================================================
# gbot — Déploiement sur le serveur Hetzner
# =============================================================================
# Transfère le code, build l'image Docker et (optionnel) lance le container.
# Le serveur doit avoir été préparé au préalable avec prepareServer.sh.
#
# Usage :
#   ./deploy.sh              # déploie le code + build Docker
#   ./deploy.sh --start      # déploie, build et (re)démarre le bot
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
START=""
for arg in "$@"; do
    case "$arg" in
        --start) START="true" ;;
        -h|--help)
            echo "Usage: $0 [--start]"
            echo ""
            echo "  (aucun flag)   Transfère le code et build l'image Docker"
            echo "  --start        Transfère, build et (re)démarre le container"
            exit 0
            ;;
        *)
            error "Option inconnue : $arg"
            echo "Usage: $0 [--start]"
            exit 1
            ;;
    esac
done

DEPLOY_DIR="/opt/gbot"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SSH_CMD="ssh gbot"

ssh_gbot() { $SSH_CMD "$@"; }

# ---- Validations locales ----
validate_local() {
    info "Vérification des prérequis locaux..."

    if [ ! -f "$SCRIPT_DIR/Cargo.toml" ]; then
        error "Cargo.toml introuvable. Êtes-vous dans le bon répertoire ?"
        exit 1
    fi

    if [ ! -f "$SCRIPT_DIR/Dockerfile" ]; then
        error "Dockerfile introuvable."
        exit 1
    fi

    if [ ! -d "$SCRIPT_DIR/static" ]; then
        error "Dossier static/ introuvable (UI du dashboard)."
        exit 1
    fi

    # Vérifier que l'alias SSH fonctionne
    if ! ssh_gbot true 2>/dev/null; then
        error "Impossible de se connecter via 'ssh gbot'."
        echo "  Vérifiez ~/.ssh/config (Host gbot) et la clé ~/.ssh/gbot"
        exit 1
    fi

    ok "Prérequis locaux OK"
}

# ---- Déployer le code ----
deploy_code() {
    info "Transfert du code vers le serveur..."

    # Créer les répertoires nécessaires
    ssh_gbot "mkdir -p ${DEPLOY_DIR}/data"

    # Transférer les fichiers du projet (sans .git, target, data, .env)
    rsync -azP --delete \
        --exclude='.git' \
        --exclude='target' \
        --exclude='data' \
        --exclude='.env' \
        --exclude='research' \
        --exclude='*.parquet' \
        -e "ssh -i ~/.ssh/gbot" \
        "$SCRIPT_DIR/" "gbot:${DEPLOY_DIR}/"

    ok "Code transféré"
}

# ---- Build Docker ----
build_image() {
    info "Build de l'image Docker sur le serveur (peut prendre quelques minutes)..."

    ssh_gbot "cd ${DEPLOY_DIR} && docker build -t gbot . 2>&1 | tail -5"

    ok "Image Docker buildée"
}

# ---- Vérifier .env ----
check_env() {
    if ! ssh_gbot "test -f ${DEPLOY_DIR}/.env" 2>/dev/null; then
        warn "Fichier .env absent sur le serveur."
        echo ""
        echo "  Créez-le sur le serveur :"
        echo "    ssh gbot"
        echo "    cat > ${DEPLOY_DIR}/.env << 'EOF'"
        echo "    GBOT__EXCHANGE__WALLET_ADDRESS=0xYOUR_WALLET"
        echo "    GBOT__EXCHANGE__AGENT_PRIVATE_KEY=YOUR_KEY"
        echo "    GBOT__GENERAL__MODE=observation"
        echo "    RUST_LOG=debug"
        echo "    EOF"
        echo "    chmod 600 ${DEPLOY_DIR}/.env"
        echo ""
        if [ "$START" = "true" ]; then
            error "Impossible de démarrer sans .env. Créez-le puis relancez avec --start."
            exit 1
        fi
    else
        ok "Fichier .env présent sur le serveur"
    fi
}

# ---- (Re)démarrer le container ----
start_container() {
    info "Vérification du container existant..."

    # Arrêter et supprimer si déjà en cours
    if ssh_gbot "docker ps -q -f name=gbot" 2>/dev/null | grep -q .; then
        warn "Container gbot en cours d'exécution — arrêt..."
        ssh_gbot "docker stop gbot && docker rm gbot"
        ok "Ancien container arrêté"
    elif ssh_gbot "docker ps -aq -f name=gbot" 2>/dev/null | grep -q .; then
        ssh_gbot "docker rm gbot" 2>/dev/null || true
    fi

    info "Démarrage du container..."
    ssh_gbot "docker run -d \
        --name gbot \
        --restart unless-stopped \
        -p 127.0.0.1:3000:3000 \
        -v ${DEPLOY_DIR}/data:/app/data \
        -v ${DEPLOY_DIR}/logs:/app/logs \
        --log-driver json-file \
        --log-opt max-size=50m \
        --log-opt max-file=10 \
        --env-file ${DEPLOY_DIR}/.env \
        gbot"

    # Vérifier que le container tourne
    sleep 2
    if ssh_gbot "docker ps -q -f name=gbot" 2>/dev/null | grep -q .; then
        ok "Container gbot démarré"

        # Health check
        sleep 3
        local health
        health=$(ssh_gbot "curl -sf http://127.0.0.1:3000/health 2>/dev/null" || echo "unreachable")
        if [ "$health" = "ok" ]; then
            ok "Health check OK"
        else
            warn "Health check: $health (le bot peut encore être en train de démarrer)"
        fi
    else
        error "Le container ne semble pas tourner. Vérifiez les logs :"
        echo "  ssh gbot 'docker logs gbot'"
        exit 1
    fi
}

# ---- Main ----
echo ""
echo "========================================="
echo "  gbot — Déploiement"
echo "========================================="
echo ""

validate_local
deploy_code
build_image
check_env

if [ "$START" = "true" ]; then
    start_container
fi

echo ""
echo "========================================="
ok "DÉPLOIEMENT TERMINÉ"
echo "========================================="
echo ""

if [ "$START" = "true" ]; then
    echo "  Bot en cours d'exécution sur le serveur."
    echo ""
    echo "  UI (tunnel SSH) :"
    echo "    ssh -L 3000:127.0.0.1:3000 gbot"
    echo "    → http://localhost:3000"
    echo ""
    echo "  Logs :"
    echo "    ssh gbot 'docker logs -f gbot'"
else
    echo "  Image buildée. Pour démarrer le bot :"
    echo "    ./deploy.sh --start"
    echo ""
    echo "  Ou manuellement sur le serveur :"
    echo "    ssh gbot"
    echo "    docker run -d --name gbot --restart unless-stopped \\"
    echo "      -p 127.0.0.1:3000:3000 \\"
    echo "      -v ${DEPLOY_DIR}/data:/app/data \\"
    echo "      --env-file ${DEPLOY_DIR}/.env gbot"
fi
echo ""
