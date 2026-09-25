# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Construction de l'interface web. Node n'existe que dans cette étape : l'image
# finale ne contient que le binaire Rust, qui embarque le résultat.
# ---------------------------------------------------------------------------
FROM node:22-alpine AS web

# La page d'aide importe `docs/notifications.md` depuis la racine du dépôt : on
# reproduit la même arborescence (`/src/web` et `/src/docs`) pour que l'import
# relatif reste identique en dev et dans l'image.
WORKDIR /src/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY docs/ /src/docs/
COPY web/ ./
RUN npm run build

# ---------------------------------------------------------------------------
# Compilation du serveur en binaire statique musl.
# ---------------------------------------------------------------------------
FROM rust:1-alpine AS builder

# musl-dev et gcc sont requis par libsqlite3-sys, qui compile SQLite depuis
# ses sources ; cela évite toute dépendance système dans l'image finale.
RUN apk add --no-cache musl-dev gcc make

WORKDIR /build

# Les dépendances sont compilées avant le code applicatif, avec des sources
# factices : le cache Docker survit alors à toute modification de nos fichiers.
COPY Cargo.toml Cargo.lock ./
COPY crates/proto/Cargo.toml crates/proto/
COPY crates/collectors/Cargo.toml crates/collectors/
COPY crates/server/Cargo.toml crates/server/
COPY crates/agent/Cargo.toml crates/agent/
# Une source factice par cible déclarée dans les manifestes — bibliothèques
# incluses, sans quoi cargo échoue avant même de compiler les dépendances.
# L'agent n'est pas compilé ici (il l'est dans l'étape `agent`, pour trois
# plateformes), mais son manifeste doit exister pour que le workspace se charge.
RUN mkdir -p crates/proto/src crates/collectors/src crates/server/src crates/agent/src \
 && echo '' > crates/proto/src/lib.rs \
 && echo '' > crates/collectors/src/lib.rs \
 && echo '' > crates/server/src/lib.rs \
 && echo 'fn main() {}' > crates/server/src/main.rs \
 && echo 'fn main() {}' > crates/agent/src/main.rs \
 && cargo build --release --locked -p dumbmonit-server \
 && rm -rf crates/proto/src crates/collectors/src crates/server/src

# La coquille de l'agent reste en place : ses vraies sources ne servent à rien
# ici, et les copier ferait recompiler le serveur à chaque retouche de l'agent.
# Seuls les scripts d'installation sont nécessaires : le serveur les incorpore
# (`include_str!`) pour les servir sur `/install.sh` et `/install.ps1`.
COPY crates/proto crates/proto
COPY crates/collectors crates/collectors
COPY crates/server crates/server
COPY crates/agent/install crates/agent/install
COPY profiles profiles
# L'interface est incorporée au binaire à la compilation : elle doit être présente
# avant `cargo build`, sinon `rust_embed` produit un serveur sans interface.
COPY --from=web /src/web/build web/build

# Sans cela, cargo réutiliserait les artefacts des sources factices.
RUN touch crates/proto/src/lib.rs crates/collectors/src/lib.rs crates/server/src/lib.rs \
      crates/server/src/main.rs \
 && cargo build --release --locked -p dumbmonit-server \
 && strip target/release/dumbmonit

# Répertoires vides recopiés dans l'image finale avec le bon propriétaire : une
# image `scratch` n'a ni `mkdir` ni `chown` (voir l'étape finale).
RUN mkdir -p /empty

# ---------------------------------------------------------------------------
# Compilation croisée de l'agent, pour les quatre plateformes que le serveur sait
# livrer sur /download/… (voir crates/server/src/api/agent_files.rs).
#
# Toujours exécutée sur la plateforme de construction : zig fait office d'éditeur
# de liens pour chaque cible, il n'y a donc rien à émuler, et l'image multi-arch
# ne compile pas quatre fois l'agent sous QEMU.
#
# macOS n'est pas ici, et ne peut pas y être : son édition de liens réclame le
# SDK d'Apple, que sa licence interdit de redistribuer. Les binaires macOS sont
# construits sur un exécuteur macOS et attachés à chaque version publiée
# (.github/workflows/release.yml) ; le serveur explique où les prendre.
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM ghcr.io/rust-cross/cargo-zigbuild:0.23.4 AS agent

# L'image embarque un rustc plus ancien que ce qu'exigent nos dépendances
# (sysinfo demande 1.95) : la chaîne est donc installée explicitement, à une
# version fixe, pour que l'agent livré ne dépende pas de la date de construction.
ARG RUST_TOOLCHAIN=1.98.0
ARG AGENT_TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu x86_64-unknown-freebsd"
RUN rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal \
      --target $(echo "$AGENT_TARGETS" | tr ' ' ',') \
 && rustup default "$RUST_TOOLCHAIN"

# L'agent tourne sur des machines modestes et se télécharge depuis le serveur :
# on privilégie la taille. Ces variables l'emportent sur le profil du workspace,
# qui reste taillé pour le serveur.
ENV CARGO_PROFILE_RELEASE_OPT_LEVEL=s \
    CARGO_PROFILE_RELEASE_LTO=fat

WORKDIR /build

# FreeBSD : zig fournit la libc, pas les autres bibliothèques de la base, dont
# `sysinfo` et `libc` réclament quatre : libgeom, libdevstat, libkvm et
# libprocstat. Chacune est remplacée ici par une bibliothèque factice au SONAME
# de la vraie (FreeBSD 13 à 15) : l'éditeur de liens inscrit la dépendance, et
# c'est la bibliothèque de la base qui est chargée sur la machine.
#
# Elles sont passées par leur chemin plutôt que par `-L` : zig fournit déjà
# des `libkvm.so` et `libdevstat.so` vides, qui passeraient devant les nôtres.
# Posées avant la compilation des dépendances : le binaire factice ci-dessous
# est déjà lié contre `sysinfo`.
COPY crates/agent/freebsd /opt/freebsd-stubs
RUN cd /opt/freebsd-stubs \
 && for lib in libgeom:5 libdevstat:7 libkvm:7 libprocstat:1; do \
      name="${lib%%:*}"; major="${lib##*:}"; \
      zig cc -target x86_64-freebsd -shared -Wl,-soname,"$name.so.$major" \
        -o "$name.so" "$name.c" || exit 1; \
    done
ENV CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_RUSTFLAGS="-L native=/opt/freebsd-stubs \
-C link-arg=/opt/freebsd-stubs/libgeom.so -C link-arg=/opt/freebsd-stubs/libdevstat.so \
-C link-arg=/opt/freebsd-stubs/libkvm.so -C link-arg=/opt/freebsd-stubs/libprocstat.so"

# Même principe que pour le serveur : dépendances d'abord, sur des sources
# factices. Le manifeste du workspace exige que chaque membre existe, d'où la
# coquille du serveur, qui n'est pas compilé ici et reste factice jusqu'au bout.
COPY Cargo.toml Cargo.lock ./
COPY crates/proto/Cargo.toml crates/proto/
COPY crates/collectors/Cargo.toml crates/collectors/
COPY crates/server/Cargo.toml crates/server/
COPY crates/agent/Cargo.toml crates/agent/
RUN mkdir -p crates/proto/src crates/collectors/src crates/server/src crates/agent/src \
 && echo '' > crates/proto/src/lib.rs \
 && echo '' > crates/collectors/src/lib.rs \
 && echo '' > crates/server/src/lib.rs \
 && echo 'fn main() {}' > crates/server/src/main.rs \
 && echo 'fn main() {}' > crates/agent/src/main.rs \
 && cargo zigbuild --release --locked -p dumbmonit-agent \
      $(for t in $AGENT_TARGETS; do echo "--target $t"; done) \
 && rm -rf crates/proto/src crates/collectors/src crates/agent/src

COPY crates/proto crates/proto
# Les collecteurs (et les profils SNMP qu'ils incorporent) : c'est ce qui permet
# à l'agent d'interroger, en mode relais, les équipements de son propre site.
COPY crates/collectors crates/collectors
COPY profiles profiles
COPY crates/agent crates/agent

# Les binaires prennent les noms exacts que les scripts d'installation demandent.
RUN touch crates/proto/src/lib.rs crates/collectors/src/lib.rs crates/agent/src/main.rs \
 && cargo zigbuild --release --locked -p dumbmonit-agent \
      $(for t in $AGENT_TARGETS; do echo "--target $t"; done) \
 && mkdir -p /agents \
 && cp target/x86_64-unknown-linux-musl/release/dumbmonit-agent  /agents/dumbmonit-agent-linux-x86_64 \
 && cp target/aarch64-unknown-linux-musl/release/dumbmonit-agent /agents/dumbmonit-agent-linux-aarch64 \
 && cp target/x86_64-pc-windows-gnu/release/dumbmonit-agent.exe  /agents/dumbmonit-agent-windows-x86_64.exe \
 && cp target/x86_64-unknown-freebsd/release/dumbmonit-agent  /agents/dumbmonit-agent-freebsd-x86_64 \
 && ls -l /agents

# ---------------------------------------------------------------------------
# Image de l'agent (ghcr.io/noekan/dumbmonit-agent) : le binaire de la
# plateforme cible et les racines TLS, rien d'autre. Construite avec
# `--target agent-image` ; l'image par défaut reste celle du serveur, plus bas.
#
# Le binaire vient de l'étape `agent`, qui compile pour toutes les plateformes
# sur la machine de construction : on choisit ici celui de la plateforme
# demandée (TARGETARCH vaut `amd64` ou `arm64`).
# ---------------------------------------------------------------------------
FROM alpine:3.21 AS agent-pick
ARG TARGETARCH
COPY --from=agent /agents /agents
RUN case "$TARGETARCH" in \
      amd64) cp /agents/dumbmonit-agent-linux-x86_64  /dumbmonit-agent ;; \
      arm64) cp /agents/dumbmonit-agent-linux-aarch64 /dumbmonit-agent ;; \
      *) echo "unsupported platform: $TARGETARCH" >&2; exit 1 ;; \
    esac

FROM scratch AS agent-image

# Racines TLS : le serveur central est en HTTPS derrière un mandataire dès
# qu'il est joint à travers l'internet, et les équipements relayés (Proxmox,
# Synology…) le sont souvent aussi. Monter un autre fichier à cet emplacement
# fait reconnaître une autorité privée. Prises dans l'étape `agent` plutôt que
# `builder` : cette image se construit alors sans compiler le serveur ni l'interface.
COPY --from=agent /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=agent-pick /dumbmonit-agent /dumbmonit-agent

# Tout se règle par l'environnement : DUMBMONIT_AGENT_URL, DUMBMONIT_AGENT_TOKEN,
# et, pour un relais, DUMBMONIT_AGENT_RELAY=true et DUMBMONIT_AGENT_SITE.
# Un fichier de configuration monté sur /etc/dumbmonit/agent.yaml est lu aussi.
# Sans socket Docker monté, la découverte des conteneurs est simplement ignorée.
ENTRYPOINT ["/dumbmonit-agent"]

# ---------------------------------------------------------------------------
# VictoriaMetrics, tel que publié par ses auteurs : un binaire Go statique. Il
# est recopié dans l'image finale et lancé par le serveur quand aucune instance
# externe n'est désignée (DUMBMONIT_VM_URL) — un seul conteneur suffit.
# Version épinglée : le serveur en connaît les options.
# ---------------------------------------------------------------------------
FROM victoriametrics/victoria-metrics:v1.152.0 AS victoriametrics

# ---------------------------------------------------------------------------
# Image finale : les deux binaires et rien d'autre.
# ---------------------------------------------------------------------------
FROM scratch

# Racines TLS pour les notifications sortantes (Discord, ntfy, SMTP) et les
# intégrations en HTTPS. Monter un autre fichier à cet emplacement permet de faire
# reconnaître une autorité de certification privée.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /build/target/release/dumbmonit /dumbmonit
# VictoriaMetrics embarqué ; `DUMBMONIT_VM_BINARY` permet d'en désigner un autre.
COPY --from=victoriametrics /victoria-metrics-prod /victoria-metrics-prod
# Binaires de l'agent, servis sur /download/… aux machines qui s'installent.
# `DUMBMONIT_AGENT_DIR` permet d'en monter d'autres à la place.
COPY --from=agent /agents /agents

# Le serveur ne tourne pas en root. L'utilisateur est numérique — l'image n'a
# pas de `/etc/passwd` — et `/data` est créé ici même, à son nom : Docker recopie
# le propriétaire d'un répertoire de l'image dans un volume nommé qu'il crée à
# la première utilisation, ce qui suffit au cas courant (`docker compose up`).
# Un bind mount ou un volume existant, eux, doivent appartenir à 65532:65532
# (voir docker-compose.yml). `/tmp` est là pour un système de fichiers racine
# monté en lecture seule ; rien n'y est écrit en fonctionnement normal.
COPY --from=builder --chown=65532:65532 /empty /data
COPY --from=builder --chown=65532:65532 /empty /tmp

ENV DUMBMONIT_BIND=0.0.0.0:8080 \
    DUMBMONIT_DATA_DIR=/data \
    DUMBMONIT_AGENT_DIR=/agents \
    DUMBMONIT_VM_BINARY=/victoria-metrics-prod

VOLUME ["/data"]
EXPOSE 8080

USER 65532:65532
ENTRYPOINT ["/dumbmonit"]
