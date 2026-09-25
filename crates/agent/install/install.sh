#!/bin/sh
# Installe l'agent système DumbMonit et son service.
#
#   curl -sSL http://serveur:8080/install.sh | sh -s -- --token=dmon_xxx --url=http://serveur:8080
#
# C'est la commande que le serveur affiche à la création d'un jeton ; le binaire
# est téléchargé sur ce même serveur, qui l'embarque dans son image — sauf pour
# macOS, dont le binaire est publié avec la version (voir plus bas).
#
# Quatre systèmes d'init, un seul script : systemd et OpenRC sous Linux, launchd
# sous macOS, rc.d sous FreeBSD. Chacun a ses chemins, son fichier de service et
# ses commandes ; tout le reste — téléchargement, empreinte, configuration,
# envoi de vérification — leur est commun.
#
# POSIX pur, sans bashisme : les NAS et les images minimales n'ont souvent que
# BusyBox ou dash, et le /bin/sh de FreeBSD n'est pas bash non plus.
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

SERVICE_NAME="dumbmonit-agent"
# Étiquette launchd et nom rc.d : ni l'un ni l'autre n'accepte le nom Linux tel
# quel — launchd veut un identifiant en domaine inversé, rc.d refuse le tiret.
LAUNCHD_LABEL="com.dumbmonit.agent"
RC_NAME="dumbmonit_agent"
LOG_FILE="/var/log/dumbmonit-agent.log"

usage() {
    cat <<'FIN'
Installs the DumbMonit system agent.

USAGE:
    install.sh --token=TOKEN [--url=URL] [OPTIONS]

OPTIONS:
    --token=TOKEN       Enrollment token (required)
    --url=URL           Server URL, for example http://server:8080
    --interval=N        Sampling period in seconds (default: 30)
    --services=a,b,c    Services whose state is reported: systemd units on
                        Linux, launchd labels on macOS, rc.d names on FreeBSD
    --tags=key=value    Tags, comma-separated
    --hostname=NAME     Name announced to the server (default: the machine's)
    --bin=PATH          Local binary to install instead of downloading it
    --no-start          Install everything, but do not contact the server or
                        start the service (machine image, testing)
    --uninstall         Uninstall the agent and delete its configuration
    --help              Show this help

Supported systems: Linux (systemd or OpenRC, x86_64 and aarch64), macOS
(launchd, Apple silicon and Intel), FreeBSD (rc.d, x86_64). On Windows, use
install.ps1 instead.

The macOS binary is not shipped in the DumbMonit image — building it requires
Apple's SDK, which cannot be redistributed. Download it from the releases page
and pass it with --bin=PATH.
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

# ------------------------------------------------- système et architecture

# Le système décide de trois choses d'un coup : le nom du binaire à
# télécharger, l'endroit où tout se range, et le gestionnaire de services.
case "$(uname -s)" in
    Linux)
        PLATFORM="linux"
        CONFIG_DIR="/etc/dumbmonit"
        BIN_PATH="/usr/local/bin/dumbmonit-agent"
        # `systemctl` peut exister sans que systemd soit l'init (conteneur,
        # chroot) : on regarde donc qui est PID 1 quand les deux outils sont
        # présents, et à défaut on se fie à l'outil disponible.
        if command -v systemctl >/dev/null 2>&1 \
           && { ! command -v rc-update >/dev/null 2>&1 || [ -d /run/systemd/system ]; }; then
            INIT="systemd"
        elif command -v rc-update >/dev/null 2>&1; then
            INIT="openrc"
        else
            INIT=""
        fi
        ;;
    Darwin)
        PLATFORM="macos"
        # `/usr/local/etc` plutôt que `/etc` : macOS réserve `/etc` au système,
        # et une mise à jour majeure y fait le ménage.
        CONFIG_DIR="/usr/local/etc/dumbmonit"
        BIN_PATH="/usr/local/bin/dumbmonit-agent"
        INIT="launchd"
        ;;
    FreeBSD)
        PLATFORM="freebsd"
        # Convention des ports FreeBSD : tout ce qui n'est pas la base vit sous
        # `/usr/local`.
        CONFIG_DIR="/usr/local/etc/dumbmonit"
        BIN_PATH="/usr/local/bin/dumbmonit-agent"
        INIT="rcd"
        ;;
    *)
        echec "unsupported system: $(uname -s).
       This script installs the agent on Linux, macOS and FreeBSD;
       use install.ps1 on Windows."
        ;;
esac

CONFIG_FILE="$CONFIG_DIR/agent.yaml"
UNIT_PATH="/etc/systemd/system/dumbmonit-agent.service"
OPENRC_PATH="/etc/init.d/dumbmonit-agent"
PLIST_PATH="/Library/LaunchDaemons/$LAUNCHD_LABEL.plist"
RC_PATH="/usr/local/etc/rc.d/$RC_NAME"

case "$(uname -m)" in
    x86_64|amd64)  ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) echec "unsupported architecture: $(uname -m)" ;;
esac

# La matrice est volontairement explicite : mieux vaut une phrase ici qu'un 404
# sur un nom de fichier qui n'a jamais existé.
if [ "$PLATFORM" = "freebsd" ] && [ "$ARCH" != "x86_64" ]; then
    echec "the FreeBSD agent is built for x86_64 only.
       Build it yourself with 'cargo build --release -p dumbmonit-agent'
       and install it with --bin=PATH."
fi

# Noms d'avant le renommage EzyMonit → DumbMonit. Une installation qui les porte
# encore est migrée sur place à l'installation, et `--uninstall` en fait aussi
# le ménage. L'ancien agent n'a existé que sous Linux.
LEGACY_CONFIG_DIR="/etc/ezymonit"
LEGACY_BIN_PATH="/usr/local/bin/ezymonit-agent"
LEGACY_UNIT_PATH="/etc/systemd/system/ezymonit-agent.service"
LEGACY_OPENRC_PATH="/etc/init.d/ezymonit-agent"
LEGACY_SERVICE_NAME="ezymonit-agent"

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

# ------------------------------------------------------------ service

arreter_service() {
    case "$INIT" in
        systemd) systemctl disable --now "$SERVICE_NAME" 2>/dev/null || true ;;
        openrc)
            rc-service "$SERVICE_NAME" stop 2>/dev/null || true
            rc-update del "$SERVICE_NAME" default 2>/dev/null || true
            ;;
        launchd)
            # `bootout` est le pendant moderne de `unload` : il retire le
            # service du domaine système, même s'il n'y est pas — d'où le `||`.
            launchctl bootout "system/$LAUNCHD_LABEL" 2>/dev/null || true
            ;;
        rcd)
            service "$RC_NAME" onestop 2>/dev/null || true
            sysrc -x "${RC_NAME}_enable" 2>/dev/null || true
            ;;
    esac
}

if [ "$UNINSTALL" -eq 1 ]; then
    info "stopping the service"
    arreter_service
    rm -f "$UNIT_PATH" "$OPENRC_PATH" "$PLIST_PATH" "$RC_PATH" "$BIN_PATH"
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

telecharger() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v fetch >/dev/null 2>&1; then
        fetch -qo "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        echec "none of curl, fetch or wget found: install one of them, or use --bin=PATH"
    fi
}

# Récupère le corps d'une réponse même en erreur : c'est là que le serveur
# explique pourquoi il ne sert pas ce binaire-là (macOS, image sans agents).
# Sans outil ou sans réponse, ne dit rien plutôt que d'inventer une raison.
expliquer_echec() {
    corps=""
    if command -v curl >/dev/null 2>&1; then
        corps="$(curl -sSL "$1" 2>/dev/null || true)"
    elif command -v fetch >/dev/null 2>&1; then
        corps="$(fetch -qo - "$1" 2>/dev/null || true)"
    elif command -v wget >/dev/null 2>&1; then
        corps="$(wget -qO - "$1" 2>/dev/null || true)"
    fi
    if [ -n "$corps" ]; then
        echo "$corps" >&2
    fi
}

install_binaire() {
    destination="$1"

    if [ -n "$LOCAL_BIN" ]; then
        [ -f "$LOCAL_BIN" ] || echec "binary not found: $LOCAL_BIN"
        info "installing from $LOCAL_BIN"
        cp "$LOCAL_BIN" "$destination"
        return
    fi

    source_url="$URL/download/dumbmonit-agent-$PLATFORM-$ARCH"
    info "downloading $source_url"
    if ! telecharger "$source_url" "$destination"; then
        rm -f "$destination"
        expliquer_echec "$source_url"
        echec "download failed from $source_url"
    fi
    verifier_empreinte "$source_url" "$destination"
}

# Le serveur publie l'empreinte SHA-256 de chaque binaire à côté de celui-ci
# (`<url>.sha256`, au format de `sha256sum`). Un binaire qui ne lui correspond
# pas — remplacé en chemin, téléchargement tronqué — n'est pas installé. Sans
# outil pour la calculer, on prévient et on continue : un système minimal ne doit
# pas être privé d'agent pour autant.
verifier_empreinte() {
    source_url="$1"
    fichier="$2"
    attendu_fichier="$fichier.sha256"
    if ! telecharger "$source_url.sha256" "$attendu_fichier" 2>/dev/null; then
        rm -f "$attendu_fichier"
        echo "Warning: no checksum published at $source_url.sha256, binary not verified" >&2
        return 0
    fi
    attendu="$(cut -d' ' -f1 "$attendu_fichier" | tr -d '[:space:]')"
    rm -f "$attendu_fichier"
    # `sha256sum` sous Linux, `shasum -a 256` sous macOS, `sha256 -q` sous
    # FreeBSD : trois noms pour la même empreinte.
    if command -v sha256sum >/dev/null 2>&1; then
        obtenu="$(sha256sum "$fichier" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        obtenu="$(shasum -a 256 "$fichier" | cut -d' ' -f1)"
    elif command -v sha256 >/dev/null 2>&1; then
        obtenu="$(sha256 -q "$fichier" | tr -d '[:space:]')"
    else
        echo "Warning: no SHA-256 tool found, binary not verified" >&2
        return 0
    fi
    if [ -z "$attendu" ] || [ "$attendu" != "$obtenu" ]; then
        rm -f "$fichier"
        echec "checksum mismatch for $source_url (expected $attendu, got $obtenu):
       the download is corrupt or has been tampered with. Nothing was installed."
    fi
    info "checksum verified ($obtenu)"
}

# Écriture à côté puis renommage : un `cp` sur un binaire en cours d'exécution
# échoue avec « Text file busy », alors qu'un renommage est atomique et sans effet
# sur le processus déjà lancé.
mkdir -p "$(dirname "$BIN_PATH")"
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
    mkdir -p "$(dirname "$UNIT_PATH")"
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
    mkdir -p "$(dirname "$OPENRC_PATH")"
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

# launchd : un démon système, pas un agent de session — il doit tourner même
# quand personne n'est connecté, et donc vivre dans /Library/LaunchDaemons.
ecrire_plist_launchd() {
    mkdir -p "$(dirname "$PLIST_PATH")"
    cat > "$PLIST_PATH" <<FIN
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LAUNCHD_LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>$BIN_PATH</string>
        <string>--config=$CONFIG_FILE</string>
    </array>
    <!-- Démarre au boot, et redémarre si l'agent s'arrête. -->
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <!-- Sans cela, un agent qui échoue au démarrage serait relancé dix fois par
         seconde jusqu'à ce que launchd s'en lasse. -->
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <!-- Même délai qu'ailleurs : l'agent vide son tampon avant d'être tué. -->
    <key>ExitTimeOut</key>
    <integer>15</integer>
    <key>StandardOutPath</key>
    <string>$LOG_FILE</string>
    <key>StandardErrorPath</key>
    <string>$LOG_FILE</string>
</dict>
</plist>
FIN
    # launchd refuse de charger un plist que quelqu'un d'autre que root peut
    # modifier — il y verrait, à raison, une porte ouverte. Le groupe « wheel »
    # existe partout sous macOS ; l'échec n'est possible que sur un système qui
    # n'en a pas, et où launchd n'existe pas non plus.
    chown root:wheel "$PLIST_PATH" 2>/dev/null || true
    chmod 0644 "$PLIST_PATH"
}

# rc.d : l'agent reste au premier plan, c'est donc daemon(8) qui le détache, le
# surveille et écrit son journal — la manière FreeBSD de faire ce que systemd
# fait tout seul.
ecrire_service_rcd() {
    mkdir -p "$(dirname "$RC_PATH")"
    cat > "$RC_PATH" <<FIN
#!/bin/sh
# DumbMonit system agent — written by install.sh.
#
# PROVIDE: $RC_NAME
# REQUIRE: LOGIN NETWORKING
# KEYWORD: shutdown

. /etc/rc.subr

name="$RC_NAME"
rcvar="${RC_NAME}_enable"
pidfile="/var/run/\${name}.pid"
command="/usr/sbin/daemon"
procname="/usr/sbin/daemon"
# -r : daemon(8) relance l'agent s'il s'arrête. -P : le pid du superviseur, que
# « service stop » doit tuer. -o : le journal, faute de journald ici.
command_args="-r -P \${pidfile} -t \${name} -o $LOG_FILE $BIN_PATH --config=$CONFIG_FILE"

load_rc_config \$name
: \${${RC_NAME}_enable:="NO"}

run_rc_command "\$1"
FIN
    chmod 0755 "$RC_PATH"
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
    launchd)
        ecrire_plist_launchd
        info "service installed at $PLIST_PATH"
        ;;
    rcd)
        ecrire_service_rcd
        info "service installed at $RC_PATH"
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
    launchd)
        # `bootout` puis `bootstrap` : c'est la seule façon de recharger un plist
        # modifié. Le premier échoue quand rien n'était chargé, ce qui est le cas
        # d'une première installation.
        launchctl bootout "system/$LAUNCHD_LABEL" 2>/dev/null || true
        launchctl bootstrap system "$PLIST_PATH"
        launchctl enable "system/$LAUNCHD_LABEL" 2>/dev/null || true
        launchctl kickstart -k "system/$LAUNCHD_LABEL"
        info "agent installed and started"
        info "status: sudo launchctl print system/$LAUNCHD_LABEL"
        info "logs:   tail -f $LOG_FILE"
        ;;
    rcd)
        # `service … enable` écrit dans /etc/rc.conf : c'est ce qui fait revenir
        # l'agent au prochain démarrage de la machine.
        service "$RC_NAME" enable >/dev/null
        service "$RC_NAME" restart
        info "agent installed and started"
        info "status: service $RC_NAME status"
        info "logs:   tail -f $LOG_FILE"
        ;;
esac
