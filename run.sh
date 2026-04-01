#!/usr/bin/env bash
set -euo pipefail
# =============================================================================
# gbot — Lancement local
# =============================================================================
# Usage :
#   ./run.sh                     # dry-run (défaut)
#   ./run.sh observation         # observation uniquement (pas de stratégie)
#   ./run.sh dry-run             # dry-run (stratégie + ordres simulés)
#   ./run.sh live                # live trading (nécessite .env)
#   ./run.sh --release           # build optimisé (n'importe quel mode)
#   ./run.sh live --release      # live en release
# =============================================================================

CYAN='\033[0;36m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ---- Arguments ----
MODE=""
RELEASE=""

for arg in "$@"; do
    case "$arg" in
        observation|dry-run|live) MODE="$arg" ;;
        --release) RELEASE="--release" ;;
        -h|--help)
            echo "Usage: $0 [MODE] [--release]"
            echo ""
            echo "  MODE:"
            echo "    observation   Collecte de données uniquement (pas de stratégie)"
            echo "    dry-run       Stratégie active, ordres simulés (défaut)"
            echo "    live          Trading réel (nécessite .env avec wallet + clé)"
            echo ""
            echo "  --release       Build optimisé (recommandé pour live)"
            exit 0
            ;;
        *) error "Option inconnue : $arg"; exit 1 ;;
    esac
done

# Défaut : dry-run
MODE="${MODE:-dry-run}"

# ---- Charger .env si présent ----
ENV_FILE=".env"
if [ -f "$ENV_FILE" ]; then
    info "Chargement de $ENV_FILE"
    set -a
    source "$ENV_FILE"
    set +a
fi

# ---- Vérifications ----
if [ "$MODE" = "live" ]; then
    if [ -z "${GBOT__EXCHANGE__WALLET_ADDRESS:-}" ]; then
        error "GBOT__EXCHANGE__WALLET_ADDRESS non défini. Créez un fichier .env :"
        echo "  GBOT__EXCHANGE__WALLET_ADDRESS=0x..."
        echo "  GBOT__EXCHANGE__AGENT_PRIVATE_KEY=..."
        exit 1
    fi
    if [ -z "${GBOT__EXCHANGE__AGENT_PRIVATE_KEY:-}" ]; then
        error "GBOT__EXCHANGE__AGENT_PRIVATE_KEY non défini."
        exit 1
    fi
    warn "MODE LIVE — ordres réels sur Hyperliquid"
fi

# ---- Créer les répertoires data ----
mkdir -p data/{l2,trades,features,signals,orders,fills,pnl,journal,logs}

# ---- Lancement ----
export GBOT__GENERAL__MODE="$MODE"
export RUST_LOG="${RUST_LOG:-debug}"

echo ""
echo "========================================="
echo "  gbot — $MODE"
[ -n "$RELEASE" ] && echo "  (release build)"
echo "========================================="
echo ""
info "Mode : $MODE"
info "Log level : $RUST_LOG"
info "Data dir : ./data"
info "UI : http://localhost:3000"
echo ""

exec cargo run $RELEASE
