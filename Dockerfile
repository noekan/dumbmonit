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
COPY crates/server/Cargo.toml crates/server/
COPY crates/agent/Cargo.toml crates/agent/
# Une source factice par cible déclarée dans les manifestes — bibliothèques
# incluses, sans quoi cargo échoue avant même de compiler les dépendances.
# L'agent n'est pas compilé ici (il l'est dans l'étape `agent`, pour trois
# plateformes), mais son manifeste doit exister pour que le workspace se charge.
RUN mkdir -p crates/proto/src crates/server/src crates/agent/src \
 && echo '' > crates/proto/src/lib.rs \
 && echo '' > crates/server/src/lib.rs \
 && echo 'fn main() {}' > crates/server/src/main.rs \
 && echo 'fn main() {}' > crates/agent/src/main.rs \
 && cargo build --release --locked -p ezymonit-server \
 && rm -rf crates/proto/src crates/server/src

# La coquille de l'agent reste en place : ses vraies sources ne servent à rien
# ici, et les copier ferait recompiler le serveur à chaque retouche de l'agent.
# Seuls les scripts d'installation sont nécessaires : le serveur les incorpore
# (`include_str!`) pour les servir sur `/install.sh` et `/install.ps1`.
COPY crates/proto crates/proto
COPY crates/server crates/server
COPY crates/agent/install crates/agent/install
COPY profiles profiles
# L'interface est incorporée au binaire à la compilation : elle doit être présente
# avant `cargo build`, sinon `rust_embed` produit un serveur sans interface.
COPY --from=web /src/web/build web/build

# Sans cela, cargo réutiliserait les artefacts des sources factices.
RUN touch crates/proto/src/lib.rs crates/server/src/lib.rs crates/server/src/main.rs \
 && cargo build --release --locked -p ezymonit-server \
 && strip target/release/ezymonit

# ---------------------------------------------------------------------------
# Compilation croisée de l'agent, pour les trois plateformes que le serveur sait
# livrer sur /download/… (voir crates/server/src/api/agent_files.rs).
#
# Toujours exécutée sur la plateforme de construction : zig fait office d'éditeur
# de liens pour chaque cible, il n'y a donc rien à émuler, et l'image multi-arch
# ne compile pas trois fois l'agent sous QEMU.
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM ghcr.io/rust-cross/cargo-zigbuild:0.23.4 AS agent

# L'image embarque un rustc plus ancien que ce qu'exigent nos dépendances
# (sysinfo demande 1.95) : la chaîne est donc installée explicitement, à une
# version fixe, pour que l'agent livré ne dépende pas de la date de construction.
ARG RUST_TOOLCHAIN=1.98.0
ARG AGENT_TARGETS="x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu"
RUN rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal \
      --target $(echo "$AGENT_TARGETS" | tr ' ' ',') \
 && rustup default "$RUST_TOOLCHAIN"

# L'agent tourne sur des machines modestes et se télécharge depuis le serveur :
# on privilégie la taille. Ces variables l'emportent sur le profil du workspace,
# qui reste taillé pour le serveur.
ENV CARGO_PROFILE_RELEASE_OPT_LEVEL=s \
    CARGO_PROFILE_RELEASE_LTO=fat

WORKDIR /build

# Même principe que pour le serveur : dépendances d'abord, sur des sources
# factices. Le manifeste du workspace exige que chaque membre existe, d'où la
# coquille du serveur, qui n'est pas compilé ici et reste factice jusqu'au bout.
COPY Cargo.toml Cargo.lock ./
COPY crates/proto/Cargo.toml crates/proto/
COPY crates/server/Cargo.toml crates/server/
COPY crates/agent/Cargo.toml crates/agent/
RUN mkdir -p crates/proto/src crates/server/src crates/agent/src \
 && echo '' > crates/proto/src/lib.rs \
 && echo '' > crates/server/src/lib.rs \
 && echo 'fn main() {}' > crates/server/src/main.rs \
 && echo 'fn main() {}' > crates/agent/src/main.rs \
 && cargo zigbuild --release --locked -p ezymonit-agent \
      $(for t in $AGENT_TARGETS; do echo "--target $t"; done) \
 && rm -rf crates/proto/src crates/agent/src

COPY crates/proto crates/proto
COPY crates/agent crates/agent

# Les binaires prennent les noms exacts que les scripts d'installation demandent.
RUN touch crates/proto/src/lib.rs crates/agent/src/main.rs \
 && cargo zigbuild --release --locked -p ezymonit-agent \
      $(for t in $AGENT_TARGETS; do echo "--target $t"; done) \
 && mkdir -p /agents \
 && cp target/x86_64-unknown-linux-musl/release/ezymonit-agent  /agents/ezymonit-agent-linux-x86_64 \
 && cp target/aarch64-unknown-linux-musl/release/ezymonit-agent /agents/ezymonit-agent-linux-aarch64 \
 && cp target/x86_64-pc-windows-gnu/release/ezymonit-agent.exe  /agents/ezymonit-agent-windows-x86_64.exe \
 && ls -l /agents

# ---------------------------------------------------------------------------
# Image finale : le binaire et rien d'autre.
# ---------------------------------------------------------------------------
FROM scratch

# Racines TLS pour les notifications sortantes (Discord, ntfy, SMTP) et les
# intégrations en HTTPS. Monter un autre fichier à cet emplacement permet de faire
# reconnaître une autorité de certification privée.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /build/target/release/ezymonit /ezymonit
# Binaires de l'agent, servis sur /download/… aux machines qui s'installent.
# `EZYMONIT_AGENT_DIR` permet d'en monter d'autres à la place.
COPY --from=agent /agents /agents

ENV EZYMONIT_BIND=0.0.0.0:8080 \
    EZYMONIT_DATA_DIR=/data \
    EZYMONIT_AGENT_DIR=/agents

VOLUME ["/data"]
EXPOSE 8080

ENTRYPOINT ["/ezymonit"]
