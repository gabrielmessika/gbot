#!/usr/bin/env bash
set -euo pipefail
# =============================================================================
# gbot.sh — Gestion du bot sur le serveur (à exécuter en SSH)
# =============================================================================
# Usage :
#   ./gbot.sh start              Démarre le bot (ou redémarre si déjà actif)
#   ./gbot.sh stop               Arrête le bot
#   ./gbot.sh restart            Redémarre le bot
#   ./gbot.sh update             Pull le code, rebuild l'image, redémarre
#   ./gbot.sh status             Affiche l'état du container + health check
#   ./gbot.sh logs [N]           Affiche les N dernières lignes de logs (défaut: 100)
#   ./gbot.sh logs -f            Suit les logs en temps réel
# =============================================================================

DEPLOY_DIR="/opt/gbot"
CONTAINER="gbot"
IMAGE="gbot"

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

# ---- Helpers ----
is_running() {
    docker ps -q -f "name=${CONTAINER}" 2>/dev/null | grep -q .
}

container_exists() {
    docker ps -aq -f "name=${CONTAINER}" 2>/dev/null | grep -q .
}

health_check() {
    local health
    health=$(curl -sf http://127.0.0.1:3000/health 2>/dev/null || echo "unreachable")
    if [ "$health" = "ok" ]; then
        ok "Health check: OK"
    else
        warn "Health check: $health"
    fi
}

# ---- Commands ----
cmd_stop() {
    if is_running; then
        info "Arrêt du container ${CONTAINER}..."
        docker stop "${CONTAINER}" >/dev/null
        docker rm "${CONTAINER}" >/dev/null 2>&1 || true
        ok "Bot arrêté"
    elif container_exists; then
        docker rm "${CONTAINER}" >/dev/null 2>&1 || true
        ok "Container nettoyé (était déjà arrêté)"
    else
        warn "Le bot n'est pas en cours d'exécution"
    fi
}

cmd_start() {
    # Nettoyer un éventuel container arrêté
    if is_running; then
        warn "Le bot tourne déjà — redémarrage..."
        cmd_stop
    elif container_exists; then
        docker rm "${CONTAINER}" >/dev/null 2>&1 || true
    fi

    # Vérifier .env
    if [ ! -f "${DEPLOY_DIR}/.env" ]; then
        error "Fichier .env absent (${DEPLOY_DIR}/.env)"
        echo "  Créez-le avec GBOT__EXCHANGE__WALLET_ADDRESS, GBOT__EXCHANGE__AGENT_PRIVATE_KEY, etc."
        exit 1
    fi

    info "Démarrage du bot..."
    docker run -d \
        --name "${CONTAINER}" \
        --restart unless-stopped \
        -p 3000:3000 \
        -v "${DEPLOY_DIR}/data:/app/data" \
        -v "${DEPLOY_DIR}/logs:/app/logs" \
        --log-driver json-file \
        --log-opt max-size=50m \
        --log-opt max-file=10 \
        --env-file "${DEPLOY_DIR}/.env" \
        "${IMAGE}" >/dev/null

    sleep 2
    if is_running; then
        ok "Bot démarré"
        sleep 3
        health_check
    else
        error "Le container n'a pas démarré. Vérifiez les logs :"
        echo "  docker logs ${CONTAINER}"
        exit 1
    fi
}

cmd_restart() {
    cmd_stop
    cmd_start
}

cmd_update() {
    info "Mise à jour du bot..."

    # Vérifier que le Dockerfile existe
    if [ ! -f "${DEPLOY_DIR}/Dockerfile" ]; then
        error "Dockerfile introuvable dans ${DEPLOY_DIR}/"
        echo "  Lancez d'abord ./deploy.sh depuis votre machine de dev."
        exit 1
    fi

    # Build la nouvelle image
    info "Build de l'image Docker (peut prendre quelques minutes)..."
    cd "${DEPLOY_DIR}"
    docker build -t "${IMAGE}" . 2>&1 | tail -5
    ok "Image reconstruite"

    # Nettoyage des anciennes images non taguées
    docker image prune -f >/dev/null 2>&1 || true

    # Redémarrer si le bot tournait
    if is_running || container_exists; then
        info "Redémarrage avec la nouvelle image..."
        cmd_restart
    else
        ok "Image mise à jour. Lancez './gbot.sh start' pour démarrer."
    fi
}

cmd_status() {
    echo ""
    if is_running; then
        ok "Bot en cours d'exécution"
        echo ""
        docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" -f "name=${CONTAINER}"
        echo ""
        health_check
    elif container_exists; then
        warn "Container existe mais est arrêté"
        docker ps -a --format "table {{.Names}}\t{{.Status}}" -f "name=${CONTAINER}"
    else
        warn "Bot non démarré (aucun container)"
    fi
    echo ""
}

cmd_logs() {
    if ! container_exists; then
        error "Aucun container ${CONTAINER} trouvé"
        exit 1
    fi

    if [ "${1:-}" = "-f" ]; then
        docker logs -f "${CONTAINER}"
    else
        local lines="${1:-100}"
        docker logs --tail "${lines}" "${CONTAINER}"
    fi
}

# ---- Main ----
if [ $# -lt 1 ]; then
    echo "Usage: $0 {start|stop|restart|update|status|logs}"
    echo ""
    echo "  start       Démarre le bot (ou redémarre si déjà actif)"
    echo "  stop        Arrête le bot"
    echo "  restart     Redémarre le bot"
    echo "  update      Rebuild l'image Docker et redémarre"
    echo "  status      Affiche l'état du container + health check"
    echo "  logs [N]    Dernières N lignes de logs (défaut: 100)"
    echo "  logs -f     Suit les logs en temps réel"
    exit 1
fi

CMD="$1"
shift

case "$CMD" in
    start)   cmd_start ;;
    stop)    cmd_stop ;;
    restart) cmd_restart ;;
    update)  cmd_update ;;
    status)  cmd_status ;;
    logs)    cmd_logs "$@" ;;
    *)
        error "Commande inconnue : $CMD"
        echo "Usage: $0 {start|stop|restart|update|status|logs}"
        exit 1
        ;;
esac
