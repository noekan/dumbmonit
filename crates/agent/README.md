# Agent système EzyMonit

Petit binaire installé sur la machine à surveiller. Il mesure le processeur, la
mémoire, les disques, le réseau, les services et les conteneurs, puis **pousse**
ses relevés vers le serveur EzyMonit en HTTP.

Le sens de connexion est délibéré : c'est l'agent qui appelle le serveur, jamais
l'inverse. Il traverse donc les NAT et les pare-feux domestiques, et n'impose
d'ouvrir **aucun port** sur la machine surveillée.

## Installation

### Linux (systemd)

```sh
curl -sSL http://serveur:8080/install.sh | sh -s -- --token=ezym_xxx --url=http://serveur:8080
```

### Windows (service)

```powershell
& ([scriptblock]::Create((irm http://serveur:8080/install.ps1))) -Token ezym_xxx -Url http://serveur:8080
```

Le jeton s'obtient dans l'interface, ou par l'API :

```sh
curl -X POST http://serveur:8080/api/agent/tokens \
     -H 'content-type: application/json' \
     -d '{"name":"parc maison","base_url":"http://serveur:8080"}'
```

La réponse contient le jeton en clair **une seule fois** : le serveur n'en garde
que l'empreinte. Elle contient aussi les deux commandes d'installation toutes
faites.

## Configuration

Fichier YAML, `/etc/ezymonit/agent.yaml` sur Linux,
`C:\ProgramData\EzyMonit\agent.yaml` sur Windows :

```yaml
server_url: http://serveur:8080
token: ezym_...
interval_secs: 30          # période d'échantillonnage
hostname: nas-cave         # facultatif : nom annoncé au serveur
services:                  # unités systemd ou services Windows
  - sshd
  - docker
tags:                      # étiquettes libres, préfixées « tag_ » côté serveur
  role: nas
  salle: cave
docker: true               # inventaire des conteneurs (défaut : true)
docker_socket: /var/run/docker.sock
max_buffered_samples: 20000
log_level: info
```

Chaque clé se surcharge par l'environnement, ce qui rend l'agent utilisable en
conteneur sans monter de fichier :

| Variable | Effet |
| --- | --- |
| `EZYMONIT_AGENT_CONFIG` | Chemin du fichier de configuration |
| `EZYMONIT_AGENT_URL` | URL du serveur |
| `EZYMONIT_AGENT_TOKEN` | Jeton d'enregistrement |
| `EZYMONIT_AGENT_INTERVAL_SECS` | Période d'échantillonnage |
| `EZYMONIT_AGENT_HOSTNAME` | Nom annoncé au serveur |
| `EZYMONIT_AGENT_SERVICES` | Services à surveiller, séparés par des virgules |
| `EZYMONIT_AGENT_TAGS` | `cle=valeur`, séparés par des virgules |
| `EZYMONIT_AGENT_DOCKER` | `true` / `false` |
| `EZYMONIT_AGENT_DOCKER_SOCKET` | Chemin du socket Docker |
| `EZYMONIT_AGENT_DOCKER_MAX_CONTAINERS` | Conteneurs détaillés par hôte, `200` par défaut (`0` : décomptes seuls) |
| `EZYMONIT_AGENT_INTERFACES_IGNORE` | Interfaces ignorées (noms ou regex, séparés par des virgules) ; par défaut `^(veth|br-|docker|virbr|lo$|vEthernet)` |
| `EZYMONIT_AGENT_INTERFACES_ONLY` | Interfaces à garder ; remplace la liste d'exclusion quand elle est définie |
| `EZYMONIT_AGENT_MOUNTS_IGNORE` | Points de montage exclus des systèmes de fichiers et des E/S disque |
| `EZYMONIT_AGENT_CPU_PER_CORE` | `true` pour envoyer aussi une série par cœur (`false` par défaut) |
| `EZYMONIT_AGENT_MAX_BUFFERED_SAMPLES` | Taille du tampon de reprise |
| `EZYMONIT_AGENT_LOG` | `trace`, `debug`, `info`, `warn`, `error` |

## Diagnostic

```sh
ezymonit-agent --dry-run          # affiche les mesures, n'envoie rien
ezymonit-agent --once             # envoie un seul lot puis s'arrête
journalctl -u ezymonit-agent -f   # journal du service
```

Le jeton n'apparaît jamais dans les journaux, pas même tronqué, y compris dans
les messages d'erreur du client HTTP.

## Ce que l'agent remonte

| Famille | Métriques | Étiquettes |
| --- | --- | --- |
| Processeur | `cpu_usage_percent`, `cpu_core_usage_percent`, `cpu_count`, `load_average_{1,5,15}` | `core` |
| Mémoire | `memory_{total,used,available}_bytes`, `memory_used_percent`, `swap_{total,used}_bytes`, `swap_used_percent` | — |
| Systèmes de fichiers | `filesystem_{total,used,free}_bytes`, `filesystem_used_percent` | `mountpoint`, `device`, `fstype` |
| Réseau (compteurs) | `if_octets_{in,out}`, `if_packets_{in,out}`, `if_errors_{in,out}` | `ifname` |
| Disques (compteurs) | `disk_read_bytes`, `disk_written_bytes`, une série par périphérique | `device` |
| Hôte | `uptime_seconds`, `process_count` | — |
| Services | `service_up` (1 = en marche) | `service` |
| Conteneurs | `container_up`, `container_count`, `container_running_count` | `container`, `image` |
| Agent | `agent_collect_seconds`, `agent_buffered_samples`, `agent_dropped_samples` | — |

Les compteurs cumulatifs partent **bruts**, en `Counter` : le taux est calculé à
la lecture. Un agent qui calculerait lui-même des débits devrait garder un état
entre deux cycles, et c'est cet état qui mentirait au premier redémarrage.

Les étiquettes d'identité (`target`, `host`, `tag_*`) sont posées par le serveur
à la réception, jamais par l'agent : une machine ne peut donc pas écrire dans les
séries d'une autre, même en forgeant ses étiquettes.

## Comportement en cas de coupure

Quand le serveur est injoignable, l'agent continue de mesurer et garde ses
relevés en mémoire, avec leur horodatage d'origine. Ils repartent dès que la
liaison revient : un redémarrage de serveur ne creuse pas de trou dans les
graphes.

Le tampon est borné (`max_buffered_samples`, environ une heure par défaut) et
sacrifie les mesures les plus anciennes quand il déborde — l'outil de
surveillance ne doit jamais devenir la panne.

Les tentatives d'envoi sont espacées par un délai qui double à chaque échec, de 5
secondes à 5 minutes, avec une part aléatoire : cent agents ne se jettent pas
tous sur le serveur à la milliseconde où il redémarre.

## Compilation

### Poste de développement

```sh
cargo build -p ezymonit-agent
cargo test  -p ezymonit-agent
cargo clippy -p ezymonit-agent --all-targets -- -D warnings
```

### Linux x86_64, binaire statique

C'est la cible native de l'image de développement du projet.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build -p ezymonit-agent --release --target x86_64-unknown-linux-musl
# cible/x86_64-unknown-linux-musl/release/ezymonit-agent
```

### Linux ARM64, binaire statique

Pour un Raspberry Pi 4/5, un NAS ARM, une machine virtuelle Ampere.

```sh
rustup target add aarch64-unknown-linux-musl
# Un éditeur de liens croisé est nécessaire : `aarch64-linux-musl-gcc`
# (musl.cc) ou le paquet `gcc-aarch64-linux-gnu` de la distribution.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc
cargo build -p ezymonit-agent --release --target aarch64-unknown-linux-musl
```

Le plus simple reste `cross`, qui apporte son propre conteneur de compilation :

```sh
cargo install cross
cross build -p ezymonit-agent --release --target aarch64-unknown-linux-musl
```

`aws-lc-rs`, tiré par `rustls`, se compile en C : la compilation croisée exige
donc un `cmake` et un compilateur C pour la cible. `cross` les fournit ; une
compilation croisée à la main les demande explicitement.

### Windows x86_64

```sh
# Depuis Windows, avec les outils de compilation Visual Studio :
cargo build -p ezymonit-agent --release --target x86_64-pc-windows-msvc

# Depuis Linux, avec MinGW-w64 :
rustup target add x86_64-pc-windows-gnu
cargo build -p ezymonit-agent --release --target x86_64-pc-windows-gnu
```

**État connu :** la compilation Windows n'a pas pu être exercée dans l'image de
développement du projet (Alpine musl, sans MinGW ni cible Windows installée). Le
code y est structuré pour : tout ce qui est propre à une plateforme est isolé
derrière `#[cfg(unix)]` / `#[cfg(windows)]` — `collect/services.rs`,
`collect/docker.rs`, `shutdown.rs` et `winsvc.rs`. Les parties Windows
(interrogation du gestionnaire de services, point d'entrée du service) restent à
compiler et à vérifier sur une machine Windows.

## Empreinte

Runtime Tokio mono-fil, journalisation sans moteur d'expressions régulières,
rafraîchissement système limité à ce qui est publié, et pas de client Docker
dédié — le dialogue HTTP sur le socket tient en quelques dizaines de lignes.
L'unité systemd installée pose en plus `MemoryMax=128M` et `CPUQuota=20%` : même
un défaut de l'agent ne peut pas emporter la machine qu'il surveille.
