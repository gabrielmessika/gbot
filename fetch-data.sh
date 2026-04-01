#!/usr/bin/env bash
set -euo pipefail
# =============================================================================
# gbot — Récupération des données du serveur pour analyse locale
# =============================================================================
# Télécharge les données d'observation, logs, signaux, ordres, fills, P&L
# depuis le serveur Hetzner vers un dossier local server-data/.
#
# Usage :
#   ./fetch-data.sh                    # tout (dernières 24h par défaut)
#   ./fetch-data.sh --all              # tout sans filtre de date
#   ./fetch-data.sh --date 2026-04-01  # données d'une date précise
#   ./fetch-data.sh --days 3           # 3 derniers jours
#   ./fetch-data.sh --logs-only        # uniquement les logs Docker
#   ./fetch-data.sh --dry-run          # affiche ce qui serait téléchargé
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
MODE="recent"
DATE_FILTER=""
DAYS=1
LOGS_ONLY=""
DRY_RUN=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --all)       MODE="all"; shift ;;
        --date)      MODE="date"; DATE_FILTER="$2"; shift 2 ;;
        --days)      MODE="days"; DAYS="$2"; shift 2 ;;
        --logs-only) LOGS_ONLY="true"; shift ;;
        --dry-run)   DRY_RUN="true"; shift ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "  (aucun flag)          Dernières 24h"
            echo "  --all                 Toutes les données"
            echo "  --date YYYY-MM-DD     Données d'une date précise"
            echo "  --days N              N derniers jours"
            echo "  --logs-only           Uniquement les logs Docker"
            echo "  --dry-run             Affiche sans télécharger"
            exit 0
            ;;
        *) error "Option inconnue : $1"; exit 1 ;;
    esac
done

REMOTE_DIR="/opt/gbot/data"
LOCAL_DIR="./server-data"
SSH_CMD="ssh gbot"

ssh_gbot() { $SSH_CMD "$@"; }

# ---- Vérification connexion ----
if ! ssh_gbot true 2>/dev/null; then
    error "Impossible de se connecter via 'ssh gbot'."
    echo "  Vérifiez ~/.ssh/config et la clé ~/.ssh/gbot"
    exit 1
fi

echo ""
echo "========================================="
echo "  gbot — Récupération des données"
echo "========================================="
echo ""

# ---- Créer la structure locale ----
mkdir -p "$LOCAL_DIR"/{l2,trades,features,signals,orders,fills,pnl,journal,logs}

# ---- Construire les filtres rsync ----
build_include_filter() {
    local filter_file
    filter_file=$(mktemp)

    case "$MODE" in
        all)
            echo "- Toutes les données" >&2
            # Pas de filtre — tout prendre
            echo "+ *" > "$filter_file"
            ;;
        date)
            info "Filtre : date = $DATE_FILTER"
            echo "+ */" > "$filter_file"
            echo "+ *${DATE_FILTER}*" >> "$filter_file"
            echo "- *" >> "$filter_file"
            ;;
        days)
            info "Filtre : $DAYS derniers jours"
            echo "+ */" > "$filter_file"
            for i in $(seq 0 $((DAYS - 1))); do
                d=$(date -d "-${i} days" +%Y-%m-%d 2>/dev/null || date -v-${i}d +%Y-%m-%d 2>/dev/null)
                echo "+ *${d}*" >> "$filter_file"
            done
            echo "- *" >> "$filter_file"
            ;;
        recent)
            info "Filtre : dernières 24h"
            local today yesterday
            today=$(date +%Y-%m-%d)
            yesterday=$(date -d "-1 day" +%Y-%m-%d 2>/dev/null || date -v-1d +%Y-%m-%d 2>/dev/null)
            echo "+ */" > "$filter_file"
            echo "+ *${today}*" >> "$filter_file"
            echo "+ *${yesterday}*" >> "$filter_file"
            echo "- *" >> "$filter_file"
            ;;
    esac

    echo "$filter_file"
}

# ---- Télécharger les logs Docker ----
fetch_logs() {
    info "Récupération des logs Docker..."

    if [ "$DRY_RUN" = "true" ]; then
        echo "  [dry-run] docker logs gbot → $LOCAL_DIR/logs/"
        return
    fi

    # Logs actuels (dernières 10000 lignes)
    ssh_gbot "docker logs --tail 10000 gbot 2>&1" > "$LOCAL_DIR/logs/gbot-$(date +%Y-%m-%d_%H%M%S).log" 2>/dev/null || {
        warn "Impossible de récupérer les logs (container arrêté ?)"
        return
    }

    local log_size
    log_size=$(wc -c < "$LOCAL_DIR/logs/"gbot-*.log 2>/dev/null | tail -1)
    ok "Logs récupérés ($(numfmt --to=iec "$log_size" 2>/dev/null || echo "${log_size} bytes"))"
}

# ---- Télécharger les données ----
fetch_data() {
    local subdir="$1"
    local desc="$2"

    # Vérifier que le dossier existe sur le serveur
    if ! ssh_gbot "test -d ${REMOTE_DIR}/${subdir}" 2>/dev/null; then
        warn "${desc} : dossier ${subdir}/ absent sur le serveur"
        return
    fi

    # Compter les fichiers
    local count
    count=$(ssh_gbot "find ${REMOTE_DIR}/${subdir} -type f | wc -l" 2>/dev/null || echo "0")
    if [ "$count" = "0" ]; then
        warn "${desc} : aucun fichier"
        return
    fi

    info "${desc} (${count} fichiers sur le serveur)..."

    if [ "$DRY_RUN" = "true" ]; then
        ssh_gbot "find ${REMOTE_DIR}/${subdir} -type f -name '*.parquet' -o -name '*.jsonl' -o -name '*.json'" 2>/dev/null | head -10
        echo "  [dry-run] → $LOCAL_DIR/${subdir}/"
        return
    fi

    local filter_file
    if [ "$MODE" = "all" ]; then
        rsync -azP \
            -e "ssh -i ~/.ssh/gbot" \
            "gbot:${REMOTE_DIR}/${subdir}/" "$LOCAL_DIR/${subdir}/"
    else
        filter_file=$(build_include_filter)
        rsync -azP \
            --filter="merge ${filter_file}" \
            -e "ssh -i ~/.ssh/gbot" \
            "gbot:${REMOTE_DIR}/${subdir}/" "$LOCAL_DIR/${subdir}/"
        rm -f "$filter_file"
    fi

    local local_count
    local_count=$(find "$LOCAL_DIR/${subdir}" -type f 2>/dev/null | wc -l)
    ok "${desc} : ${local_count} fichiers téléchargés"
}

# ---- Récupérer le snapshot courant de l'API ----
fetch_api_snapshot() {
    info "Snapshot API /api/state..."

    if [ "$DRY_RUN" = "true" ]; then
        echo "  [dry-run] curl /api/state → $LOCAL_DIR/api-state.json"
        return
    fi

    ssh_gbot "curl -sf http://127.0.0.1:3000/api/state 2>/dev/null" \
        > "$LOCAL_DIR/api-state-$(date +%Y-%m-%d_%H%M%S).json" 2>/dev/null || {
        warn "API inaccessible (bot arrêté ?)"
        return
    }
    ok "Snapshot API sauvegardé"
}

# ---- Main ----

# Toujours récupérer les logs
fetch_logs

if [ "$LOGS_ONLY" = "true" ]; then
    echo ""
    ok "Logs récupérés dans $LOCAL_DIR/logs/"
    exit 0
fi

# Snapshot API (si le bot tourne)
fetch_api_snapshot

# Données par catégorie
fetch_data "l2"        "Book L2"
fetch_data "trades"    "Trade tape"
fetch_data "features"  "Features calculées"
fetch_data "signals"   "Signaux"
fetch_data "orders"    "Ordres"
fetch_data "fills"     "Fills"
fetch_data "pnl"       "P&L timeline"
fetch_data "journal"   "Journal (JSONL)"

# ---- Résumé ----
echo ""
echo "========================================="
ok "RÉCUPÉRATION TERMINÉE"
echo "========================================="
echo ""

# Taille totale
total_size=$(du -sh "$LOCAL_DIR" 2>/dev/null | awk '{print $1}')
total_files=$(find "$LOCAL_DIR" -type f 2>/dev/null | wc -l)
echo "  Dossier : $LOCAL_DIR/"
echo "  Fichiers : $total_files"
echo "  Taille : $total_size"
echo ""
echo "  Analyse avec DuckDB :"
echo "    duckdb -c \"SELECT * FROM '$LOCAL_DIR/signals/*.parquet' LIMIT 10\""
echo ""
echo "  Ou Python :"
echo "    import polars as pl"
echo "    df = pl.read_parquet('$LOCAL_DIR/fills/*.parquet')"
echo ""
