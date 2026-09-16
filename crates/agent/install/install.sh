#!/bin/sh
# Installe l'agent système DumbMonit et son service (systemd ou OpenRC).
#
#   curl -sSL http://serveur:8080/install.sh | sh -s -- --token=dmon_xxx --url=http://serveur:8080
#
# C'est la commande que le serveur affiche à la création d'un jeton ; le binaire
# est téléchargé sur ce même serveur, qui l'embarque dans son image.
#
# POSIX pur, sans bashisme : les NAS et les images minimales n'ont souvent que
# BusyBox ou dash, et un script d'installation qui exige bash n'est pas un script
# d'installation universel.
#
# Idempotent : le relancer met à jour le binaire et la configuration sans rien
# casser, ce qui en fait aussi la procédure de mise à jour. Une machine encore
# équipée de l'agent d'avant le renommage (ezymonit-agent) est migrée sur place
# par la même commande.

set -eu

# URL du serveur, à défaut de `--url=` : pratique quand le script est lancé par
# un outil de déploiement qui préfère l'environnement aux arguments.
DUMBMONIT_URL="${DUMBMONIT_URL:-}"

TOKEN=""
URL=""
INTERVAL=""
SERVICES=""
TAGS=""
HOSTNAME_OVERRIDE=""
LOCAL_BIN=""
UNINSTALL=0
NO_START=0

CONFIG_DIR="/etc/dumbmonit"
CONFIG_FILE="$CONFIG_DIR/agent.yaml"
BIN_PATH="/usr/local/bin/dumbmonit-agent"
UNIT_PATH="/etc/systemd/system/dumbmonit-agent.service"
OPENRC_PATH="/etc/init.d/dumbmonit-agent"
SERVICE_NAME="dumbmonit-agent"

# Noms d'avant le renommage EzyMonit → DumbMonit. Une installation qui les porte
# encore est migrée sur place à l'installation, et `--uninstall` en fait aussi
# le ménage.
LEGACY_CONFIG_DIR="/etc/ezymonit"
LEGACY_BIN_PATH="/usr/local/bin/ezymonit-agent"
LEGACY_UNIT_PATH="/etc/systemd/system/ezymonit-agent.service"
LEGACY_OPENRC_PATH="/etc/init.d/ezymonit-agent"
LEGACY_SERVICE_NAME="ezymonit-agent"

usage() {
    cat <<'FIN'
Installs the DumbMonit system agent.

USAGE:
    install.sh --token=TOKEN [--url=URL] [OPTIONS]

OPTIONS:
    --token=TOKEN       Enrollment token (required)
    --url=URL           Server URL, for example http://server:8080
    --interval=N        Sampling period in seconds (default: 30)
    --services=a,b,c    systemd units whose state is reported
    --tags=key=value    Tags, comma-separated
    --hostname=NAME     Name announced to the server (default: the machine's)
    --bin=PATH          Local binary to install instead of downloading it
    --no-start          Install everything, but do not contact the server or
                        start the service (machine image, testing)
    --uninstall         Uninstall the agent and delete its configuration
    --help              Show this help

The service is registered with systemd, or with OpenRC otherwise (Alpine).
FIN
}

echec() {
    echo "Error: $*" >&2
    exit 1
}

info() {
    echo "==> $*"
}

for argument in "$@"; do
    case "$argument" in
        --token=*)    TOKEN="${argument#*=}" ;;
        --url=*)      URL="${argument#*=}" ;;
        --interval=*) INTERVAL="${argument#*=}" ;;
        --services=*) SERVICES="${argument#*=}" ;;
        --tags=*)     TAGS="${argument#*=}" ;;
        --hostname=*) HOSTNAME_OVERRIDE="${argument#*=}" ;;
        --bin=*)      LOCAL_BIN="${argument#*=}" ;;
        --no-start)   NO_START=1 ;;
        --uninstall)  UNINSTALL=1 ;;
        --help|-h)    usage; exit 0 ;;
        *)            echec "unknown option '$argument' (see --help)" ;;
    esac
done

[ "$(id -u)" -eq 0 ] || echec "this installer must run as root (try with sudo)"

# Gestionnaire de services. `systemctl` peut exister sans que systemd soit
# l'init (conteneur, chroot) : on regarde donc qui est PID 1 quand les deux
# outils sont présents, et à défaut on se fie à l'outil disponible.
if command -v systemctl >/dev/null 2>&1 \
   && { ! command -v rc-update >/dev/null 2>&1 || [ -d /run/systemd/system ]; }; then
    INIT="systemd"
elif command -v rc-update >/dev/null 2>&1; then
    INIT="openrc"
else
    INIT=""
fi

# --------------------------------------------------------- ancien agent

# Vrai si le service d'avant le renommage est encore connu du gestionnaire de
# services : fichier d'unité ou script d'init présent, ou unité listée par
# systemd (elle peut venir d'ailleurs que /etc).
ancien_service_present() {
    case "$INIT" in
        systemd)
            [ -f "$LEGACY_UNIT_PATH" ] && return 0
            systemctl list-unit-files "$LEGACY_SERVICE_NAME.service" 2>/dev/null \
                | grep -q "^$LEGACY_SERVICE_NAME\.service"
            ;;
        openrc) [ -f "$LEGACY_OPENRC_PATH" ] ;;
        *) return 1 ;;
    esac
}

# Arrête et retire le service, l'unité et le binaire de l'ancien agent. Ne
# touche pas à sa configuration. Silencieux quand rien d'ancien n'est présent ;
# renvoie 0 si quelque chose a été retiré.
retirer_ancien_agent() {
    retire=1
    if ancien_service_present; then
        case "$INIT" in
            systemd) systemctl disable --now "$LEGACY_SERVICE_NAME" 2>/dev/null || true ;;
            openrc)
                rc-service "$LEGACY_SERVICE_NAME" stop 2>/dev/null || true
                rc-update del "$LEGACY_SERVICE_NAME" default 2>/dev/null || true
                ;;
        esac
        retire=0
    fi
    for fichier in "$LEGACY_UNIT_PATH" "$LEGACY_OPENRC_PATH" "$LEGACY_BIN_PATH"; do
        if [ -e "$fichier" ] || [ -L "$fichier" ]; then
            rm -f "$fichier"
            retire=0
        fi
    done
    if [ "$retire" -eq 0 ] && [ "$INIT" = "systemd" ]; then
        systemctl daemon-reload 2>/dev/null || true
    fi
    return "$retire"
}

# Migration sur place d'une installation d'avant le renommage : l'ancien service
# est arrêté et retiré, et sa configuration reprend sa place sous le nouveau nom.
# Elle est réécrite juste après depuis --token/--url, mais les clés qu'un
# utilisateur y aurait ajoutées restent à portée de main. Sans rien d'ancien,
# ne fait rien et ne dit rien.
migrate_legacy() {
    migre=1
    if retirer_ancien_agent; then migre=0; fi
    if [ -d "$LEGACY_CONFIG_DIR" ] && [ ! -e "$CONFIG_DIR" ]; then
        # `mv` garde propriétaire et droits, ceux du fichier de jeton compris.
        mv "$LEGACY_CONFIG_DIR" "$CONFIG_DIR"
        migre=0
    fi
    if [ "$migre" -eq 0 ]; then
        info "migrating the $LEGACY_SERVICE_NAME install to $SERVICE_NAME"
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    info "stopping the service"
    case "$INIT" in
        systemd) systemctl disable --now "$SERVICE_NAME" 2>/dev/null || true ;;
        openrc)
            rc-service "$SERVICE_NAME" stop 2>/dev/null || true
            rc-update del "$SERVICE_NAME" default 2>/dev/null || true
            ;;
    esac
    rm -f "$UNIT_PATH" "$OPENRC_PATH" "$BIN_PATH"
    rm -rf "$CONFIG_DIR"
    # Les restes d'une installation d'avant le renommage partent aussi.
    retirer_ancien_agent || true
    rm -rf "$LEGACY_CONFIG_DIR"
    [ "$INIT" = "systemd" ] && systemctl daemon-reload 2>/dev/null || true
    info "agent uninstalled"
    exit 0
fi

[ -n "$TOKEN" ] || echec "the enrollment token is required (--token=...)"

[ -n "$INIT" ] \
    || echec "neither systemd nor OpenRC found: run the agent by hand or adapt this script"

# À défaut d'option explicite, l'URL est celle depuis laquelle ce script a été
# téléchargé. Sans l'une ni l'autre, impossible de deviner : autant le dire.
[ -n "$URL" ] || URL="$DUMBMONIT_URL"
[ -n "$URL" ] || echec "the server URL is required (--url=http://server:8080)"
URL="${URL%/}"

# ---------------------------------------------------------------- binaire

case "$(uname -s)" in
    Linux) ;;
    *) echec "this script installs the agent on Linux; use install.ps1 on Windows" ;;
esac

case "$(uname -m)" in
    x86_64|amd64)  ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) echec "unsupported architecture: $(uname -m)" ;;
esac

install_binaire() {
    destination="$1"

    if [ -n "$LOCAL_BIN" ]; then
        [ -f "$LOCAL_BIN" ] || echec "binary not found: $LOCAL_BIN"
        info "installing from $LOCAL_BIN"
        cp "$LOCAL_BIN" "$destination"
        return
    fi

    source_url="$URL/download/dumbmonit-agent-linux-$ARCH"
    info "downloading $source_url"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$source_url" -o "$destination" \
            || echec "download failed from $source_url"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$destination" "$source_url" \
            || echec "download failed from $source_url"
    else
        echec "neither curl nor wget found: install one of them, or use --bin=PATH"
    fi
}

# Écriture à côté puis renommage : un `cp` sur un binaire en cours d'exécution
# échoue avec « Text file busy », alors qu'un renommage est atomique et sans effet
# sur le processus déjà lancé.
TMP_BIN="$BIN_PATH.nouveau"
install_binaire "$TMP_BIN"
chmod 0755 "$TMP_BIN"
mv -f "$TMP_BIN" "$BIN_PATH"

# Le nouveau binaire est en place : l'ancien agent, s'il est là, peut être
# arrêté et sa configuration déplacée avant que la nouvelle ne soit écrite. Pas
# avant — un téléchargement raté ne doit pas laisser la machine sans agent.
migrate_legacy

# --------------------------------------------------------- configuration

mkdir -p "$CONFIG_DIR"
chmod 0750 "$CONFIG_DIR"

# Le fichier est écrit avec des droits restreints AVANT d'y mettre le jeton : le
# créer en 0644 puis le restreindre laisserait une fenêtre où n'importe quel
# utilisateur de la machine peut le lire.
umask 077
: > "$CONFIG_FILE"

{
    echo "# DumbMonit system agent configuration."
    echo "# Written by install.sh — environment variables override it."
    echo "server_url: $URL"
    echo "token: $TOKEN"
    if [ -n "$INTERVAL" ]; then echo "interval_secs: $INTERVAL"; fi
    if [ -n "$HOSTNAME_OVERRIDE" ]; then echo "hostname: $HOSTNAME_OVERRIDE"; fi

    if [ -n "$SERVICES" ]; then
        echo "services:"
        echo "$SERVICES" | tr ',' '\n' | while read -r service; do
            service="$(echo "$service" | tr -d ' ')"
            if [ -n "$service" ]; then echo "  - $service"; fi
        done
    fi

    if [ -n "$TAGS" ]; then
        echo "tags:"
        echo "$TAGS" | tr ',' '\n' | while read -r pair; do
            cle="$(echo "${pair%%=*}" | tr -d ' ')"
            valeur="${pair#*=}"
            if [ -n "$cle" ]; then echo "  $cle: $valeur"; fi
        done
    fi
} >> "$CONFIG_FILE"

chmod 0600 "$CONFIG_FILE"
info "configuration written to $CONFIG_FILE"

# ------------------------------------------------------------- service

ecrire_unite_systemd() {
    cat > "$UNIT_PATH" <<FIN
[Unit]
Description=DumbMonit system agent
Documentation=https://github.com/noekan/dumbmonit
# Sans réseau, le premier envoi échouerait et l'agent temporiserait pour rien.
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
ExecStart=$BIN_PATH --config=$CONFIG_FILE
# L'agent perdrait le contenu de son tampon s'il était tué net : on lui laisse le
# temps de vider ce qu'il a gardé pendant une éventuelle coupure réseau.
KillSignal=SIGTERM
TimeoutStopSec=15
Restart=always
RestartSec=10

# L'agent tourne en root : lire l'état des unités systemd et le socket Docker le
# demande. Le durcissement ci-dessous lui retire tout le reste — il ne peut rien
# écrire hors de son propre répertoire, ni obtenir de nouveaux privilèges.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
LockPersonality=yes
SystemCallArchitectures=native

# Empreinte volontairement contrainte : l'outil de surveillance ne doit jamais
# devenir la cause de la panne qu'il est censé détecter.
MemoryMax=128M
CPUQuota=20%

[Install]
WantedBy=multi-user.target
FIN
}

ecrire_service_openrc() {
    cat > "$OPENRC_PATH" <<FIN
#!/sbin/openrc-run
# DumbMonit system agent — written by install.sh.

description="DumbMonit system agent"
command="$BIN_PATH"
command_args="--config=$CONFIG_FILE"
command_background=true
pidfile="/run/\${RC_SVCNAME}.pid"
output_log="/var/log/\${RC_SVCNAME}.log"
error_log="/var/log/\${RC_SVCNAME}.log"
# Même délai qu'avec systemd : l'agent vide son tampon avant de s'arrêter.
retry="SIGTERM/15"

depend() {
    need net
    after firewall
}
FIN
    chmod 0755 "$OPENRC_PATH"
}

case "$INIT" in
    systemd)
        ecrire_unite_systemd
        info "service installed at $UNIT_PATH"
        ;;
    openrc)
        ecrire_service_openrc
        info "service installed at $OPENRC_PATH"
        ;;
esac

# --------------------------------------------------------- vérification

if [ "$NO_START" -eq 1 ]; then
    # On s'assure au moins que le binaire est bien celui de cette architecture :
    # un « Exec format error » se découvre mieux ici qu'au premier démarrage.
    "$BIN_PATH" --version >/dev/null || echec "the installed binary does not run on this machine"
    info "agent installed, service not started (--no-start)"
    exit 0
fi

# Un envoi de test avant de démarrer le service : si le jeton est mauvais ou le
# serveur injoignable, on le dit maintenant, pas dans le journal trois jours plus
# tard.
info "checking the connection to $URL"
if ! "$BIN_PATH" --config="$CONFIG_FILE" --once; then
    echec "the agent could not reach the server — check the URL and the token.
       The configuration is in place: fix $CONFIG_FILE, then restart
       the service (see above)."
fi

case "$INIT" in
    systemd)
        systemctl daemon-reload
        systemctl enable --now "$SERVICE_NAME"
        # `enable --now` ne redémarre pas un service déjà actif : lors d'une mise
        # à jour, c'est pourtant le nouveau binaire qu'on veut voir tourner.
        systemctl restart "$SERVICE_NAME"
        info "agent installed and started"
        info "status: systemctl status $SERVICE_NAME"
        info "logs:   journalctl -u $SERVICE_NAME -f"
        ;;
    openrc)
        rc-update add "$SERVICE_NAME" default >/dev/null
        rc-service "$SERVICE_NAME" restart
        info "agent installed and started"
        info "status: rc-service $SERVICE_NAME status"
        info "logs:   tail -f /var/log/$SERVICE_NAME.log"
        ;;
esac
