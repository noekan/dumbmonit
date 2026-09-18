# Collecteur Proxmox Backup Server

Intégration de Proxmox Backup Server (PBS) : état du nœud, remplissage des
datastores, ancienneté et vérification de la dernière sauvegarde de chaque
machine, tâches en échec, nettoyage (GC), travaux planifiés (synchronisation,
vérification, purge) et mises à jour en attente. Le module est calqué sur celui
de Proxmox VE ; il ne dépend que de `dumbmonit-proto`, `reqwest` (via le client
partagé `collectors::http`), `serde`, `chrono`, `tokio` et `futures`.

## Câblage

Déjà fait, mais pour mémoire :

* `crates/server/src/collectors/mod.rs` : `mod pbs;` et `pub use pbs::PbsCollector;` ;
* `crates/server/src/main.rs` : `registry.register(Arc::new(collectors::PbsCollector::new()));` ;
* `crates/server/src/api/collectors.rs` : la notice de mise en route et `PBS_OPTIONS` ;
* `crates/server/src/alerting/rules.rs` : les règles `pbs_*`.

Les cibles de type `pbs` sont alors interrogées par le planificateur.

## Configuration d'une cible

* **Adresse** : `10.0.0.20`, `pbs.lan`, `pbs.lan:8007`, `[fd00::2]` ou une URL
  complète `https://pbs.example.net`. Le schéma `https` et le port `8007` sont
  ajoutés si besoin.
* **Identifiant** :
  * `Credential::ApiToken` — la chaîne complète affichée par PBS à la création du
    jeton : `utilisateur@pbs!nom-du-jeton=secret`. **Mode recommandé.** Le
    collecteur la convertit en en-tête `Authorization: PBSAPIToken=utilisateur@pbs!nom:secret`
    (PBS sépare l'identifiant du secret par `:`, là où PVE utilise `=`).
  * `Credential::UsernamePassword` — `utilisateur@pbs` (ou `@pam`) et le mot de
    passe. Un ticket est obtenu sur `POST /access/ticket`, transmis dans le cookie
    `PBSAuthCookie`, mis en cache et renouvelé dix minutes avant sa fin de vie
    (deux heures).
* **Droits nécessaires**, en lecture seule. Le plus simple : le rôle `Audit`
  sur `/`, plus `RemoteAudit` sur `/remote` pour voir les travaux de
  synchronisation. Le strict minimum, avec lequel le collecteur fonctionne mais
  sans mises à jour ni travaux de synchronisation :
  * `DatastoreAudit` sur `/datastore` — occupation, instantanés, statut de GC,
    travaux de vérification et de purge (les listes sont filtrées par PBS, un
    travail non visible est simplement absent) ;
  * `Audit` sur `/system` — état du nœud et liste des tâches ;
  * `Audit` sur `/` — mises à jour en attente (PBS vérifie `Sys.Audit` à la
    racine) ; un 403 est toléré : pas de série, pas d'erreur de collecte ;
  * `RemoteAudit` sur `/remote` — travaux de synchronisation (en plus de
    `Datastore.Audit` sur le datastore de destination).

  Rien de plus n'est requis, le collecteur ne fait que des `GET`.

### Certificat auto-signé

Une installation PBS par défaut présente un certificat auto-signé, que le
collecteur refuse. Le message d'erreur le dit explicitement. Deux issues :

1. installer un certificat reconnu sur le serveur (ACME est intégré à PBS) ;
2. ajouter l'étiquette `insecure_tls = true` sur la cible.

L'option n'est jamais activée implicitement.

### Étiquettes reconnues

| Étiquette | Défaut | Rôle |
|---|---|---|
| `insecure_tls` | `false` | Accepte un certificat non vérifiable. |
| `port` | `8007` | Port de l'API si l'adresse n'en précise pas. |
| `request_timeout_seconds` | `15` | Délai par requête HTTP (1 à 120). |
| `task_lookback_hours` | `24` | Fenêtre d'examen des tâches (1 à 8760). |
| `datastores` | tous | Liste de datastores à collecter, séparés par des virgules. |
| `max_groups` | `500` | Plafond de groupes de sauvegarde produisant des séries. |
| `jobs` | `true` | Interroge les listes de travaux planifiés. |
| `updates` | `true` | Interroge les mises à jour de paquets en attente. |
| `disks` | `true` | Interroge les disques physiques et les pools ZFS du nœud. |

Ces étiquettes sont aussi recopiées en `tag_*` sur les séries, par le registre.

## Endpoints interrogés

| Endpoint | Usage |
|---|---|
| `GET /version` | Sonde de vie et d'authentification ; seul appel dont l'échec fait échouer l'interrogation. |
| `GET /nodes/localhost/status` | CPU, charge, mémoire, swap, racine, uptime, noyau. |
| `GET /status/datastore-usage` | Occupation par datastore et estimation de remplissage. |
| `GET /nodes/localhost/tasks?limit=500&since=…` | Tâches de la fenêtre, par type de travail. |
| `GET /admin/datastore/{store}/gc` | Facteur de déduplication, dernière GC. |
| `GET /admin/datastore/{store}/namespace` | Espaces de noms (toléré en échec : racine seule). |
| `GET /admin/datastore/{store}/snapshots[?ns=…]` | Instantanés, regroupés par machine et comptés par espace de noms. |
| `GET /admin/sync`, `GET /admin/verify`, `GET /admin/prune` | Travaux planifiés et leur dernier passage (option `jobs` ; 403 toléré). |
| `GET /nodes/localhost/apt/update` | Mises à jour de paquets en attente (option `updates` ; 403 toléré). |
| `GET /nodes/localhost/disks/list`, `…/disks/zfs` | Disques physiques (verdict SMART, usure) et pools ZFS (option `disks` ; 403 toléré). |
| `GET /nodes/localhost/tasks/{upid}/log` | Journal d'une tâche, **à la demande** seulement (`task_log`), jamais par la sonde. |
| `GET /nodes/localhost/disks/smart?disk=…` | Détail SMART d'un disque, **à la demande** seulement (`disk_smart`). |

Les appels par datastore sont limités à quatre en parallèle : lister les
instantanés lit les index sur le disque du datastore, et une rafale ralentirait
la sauvegarde en cours.

## La vue, au-delà des métriques

Le calendrier des sauvegardes de l'interface a besoin des tâches et des
instantanés eux-mêmes. Chaque interrogation réussie construit donc une
`ProbeView` (`view.rs` : datastores avec GC et facteur de déduplication,
groupes avec leurs derniers instantanés et le nom de l'invité lu dans les
notes, travaux — GC comprise, `kind = "gc"` —, tâches, disques, pools) et la
livre à l'observateur enregistré par `PbsCollector::with_observer`. Côté
serveur, `collectors/pbs_history.rs` la range dans SQLite (`db/pbs.rs`) ; l'API
`api/pbs.rs` construit le calendrier à partir de là. Sans observateur (agent
relais, tests), rien ne change : la sonde ne produit que des métriques.

À la première interrogation d'une cible depuis le démarrage, la liste des
tâches couvre `HISTORY_DAYS` (30) jours avec `limit=5000` pour reconstituer
l'historique ; ensuite la fenêtre `task_lookback_hours` suffit. Une
interrogation dont la liste des tâches échoue refera la reconstitution la
fois suivante.

## Métriques

Toutes préfixées `pbs_` ici, `dumbmonit_pbs_` une fois écrites.

| Métrique | Étiquettes | Sens |
|---|---|---|
| `pbs_up` | | 1 si l'API a répondu. |
| `pbs_version_info` | `version`, `release`, `repoid` | Série de présence. |
| `pbs_node_cpu_percent`, `pbs_node_cpu_count`, `pbs_node_load1/5/15` | | Processeur. |
| `pbs_node_memory_used_bytes`, `_total_bytes`, `pbs_node_memory_used_percent` | | Mémoire. |
| `pbs_node_swap_used_bytes`, `_total_bytes` | | Swap. |
| `pbs_node_rootfs_used_bytes`, `_total_bytes`, `_avail_bytes`, `pbs_node_rootfs_percent` | | Système de fichiers racine. |
| `pbs_node_uptime_seconds` | | Uptime. |
| `pbs_node_kernel_info` | `kversion` | Série de présence. |
| `pbs_datastore_available` | `datastore` | 0 si PBS signale une erreur sur le datastore. |
| `pbs_datastore_bytes_used`, `_total`, `_avail`, `pbs_datastore_used_percent` | `datastore` | Occupation. |
| `pbs_datastore_estimated_full_seconds` | `datastore` | Temps restant estimé par PBS ; absent si inconnu ou si l'occupation décroît. |
| `pbs_datastore_dedup_factor` | `datastore` | Octets référencés / octets sur disque, d'après la dernière GC. |
| `pbs_gc_last_removed_bytes`, `pbs_gc_last_pending_bytes` | `datastore` | Résultat de la dernière GC. |
| `pbs_gc_last_run_ok` | `datastore` | D'après `/gc` (PBS ≥ 3.3), sinon d'après la GC la plus récente de la fenêtre de tâches. |
| `pbs_gc_last_success_age_seconds` | `datastore` | Âge de la dernière GC réussie (tâches et statut `/gc` confondus). |
| `pbs_verify_last_success_age_seconds` | `datastore` | Âge de la dernière vérification réussie, dans la fenêtre. |
| `pbs_sync_last_success_age_seconds` | `datastore` | Âge de la dernière synchronisation réussie (`sync`, `syncjob`), dans la fenêtre. |
| `pbs_backup_count` | `datastore`, `namespace`, `backup_type`, `group` | Instantanés du groupe. |
| `pbs_backup_last_timestamp_seconds`, `pbs_backup_last_age_seconds` | idem | Dernier instantané. |
| `pbs_backup_last_size_bytes` | idem | Taille du dernier instantané. |
| `pbs_backup_last_verified` | idem | 1 vérifié, 0 en échec ; absent si jamais vérifié. |
| `pbs_backup_groups_total`, `pbs_backup_groups_dropped` | | Groupes vus, groupes écartés par `max_groups`. |
| `pbs_namespace_groups`, `pbs_namespace_snapshots` | `datastore`, `namespace` | Décomptes par espace de noms listé (racine : `namespace=""`), hors plafond. |
| `pbs_tasks_running`, `pbs_tasks_ok`, `pbs_tasks_failed` | `worktype` | Tâches de la fenêtre, par type (`backup`, `verificationjob`, `garbage_collection`, `prune`, `syncjob`…). |
| `pbs_job_enabled` | `job`, `datastore`, `kind` (+ `remote` si `kind="sync"`) | 1 sauf `disable`. |
| `pbs_job_last_ok` | idem | 1 si le dernier passage est `OK` ou `WARNINGS: n`, 0 sinon ; absent si jamais exécuté. |
| `pbs_job_last_run_age_seconds` | idem | `now − last-run-endtime`, si présent. |
| `pbs_job_next_run_seconds` | idem | `next-run − now`, si présent ; négatif en retard. |
| `pbs_sync_job_last_ok` | `job`, `datastore`, `remote` | Alias de `job_last_ok` pour `kind="sync"`, sans `kind`, pour la règle. |
| `pbs_sync_jobs_total`, `pbs_verify_jobs_total`, `pbs_prune_jobs_total` | | Travaux listés (avec identifiant). |
| `pbs_node_updates_pending` | | Paquets ayant une mise à jour disponible. |
| `pbs_node_disk_size_bytes`, `pbs_node_disk_smart_failed` (0/1, absent sans verdict), `pbs_node_disk_wearout_percent` (usure consommée, SSD), `pbs_node_disk_health_info` | `disk` (`/dev/sda`), `model`, `type` (+ `health`, `serial`, `used` sur `_info`) | Mêmes noms que pour un nœud PVE. |
| `pbs_node_zfs_pool_degraded` (0 `ONLINE`, 1 sinon), `_health_info`, `_size_bytes`, `_alloc_bytes`, `_free_bytes`, `_used_percent`, `_fragmentation_percent` | `pool` (+ `health` sur `_info`) | Mêmes noms que pour un nœud PVE. |
| `pbs_scrape_errors`, `pbs_scrape_duration_seconds` | | Santé de la collecte. |

Une tâche terminée en `WARNINGS: n` compte comme réussie.

## Règles livrées

| uid | Condition | `for` | Gravité |
|---|---|---|---|
| `pbs_datastore_almost_full` | `used_percent > 90` | 15 min | warning |
| `pbs_datastore_will_be_full` | `estimated_full_seconds < 7 j` | 1 h | warning |
| `pbs_backup_too_old` | `backup_last_age_seconds > 2 j` | 1 h | warning |
| `pbs_backup_verification_failed` | `backup_last_verified < 1` | 30 min | critical |
| `pbs_task_failed` | `tasks_failed > 0` | 10 min | warning |
| `pbs_gc_too_old` | `gc_last_success_age_seconds > 8 j` | 1 h | warning |
| `pbs_sync_job_failed` | `sync_job_last_ok < 1` | — | — |
| `pbs_updates_pending` | `node_updates_pending > 0` | — | — |
| `pbs_prune_failed` | `job_last_ok{kind="prune"} < 1` | 10 min | warning |
| `pbs_verify_job_failed` | `job_last_ok{kind="verify"} < 1` | 10 min | warning |
| `pbs_gc_failed` | `gc_last_run_ok < 1` | 10 min | warning |
| `pbs_disk_smart_failed` | `node_disk_smart_failed > 0` | 5 min | critical |
| `pbs_disk_wearout` | `node_disk_wearout_percent > 90` | 1 h | warning |
| `pbs_zpool_degraded` | `node_zfs_pool_degraded > 0` | 5 min | critical |

Les deux dernières sont écrites par le coordinateur d'après le contrat de
métriques ; leurs seuils et gravités sont dans `alerting/rules.rs`.

## Organisation

| Fichier | Rôle |
|---|---|
| `mod.rs` | Le collecteur, l'orchestration et la tolérance aux pannes partielles. |
| `client.rs` | Le seul module qui fait du réseau : requêtes, statuts, erreurs. |
| `auth.rs` | En-tête de jeton (`=` → `:`), cycle de vie du ticket. |
| `options.rs` | Lecture des étiquettes, composition de l'URL de base. |
| `model.rs` | Désérialisation des réponses de l'API. |
| `metrics.rs` | Conversion nœud, datastores, GC en `Sample`. Pur, testé en totalité. |
| `backup.rs` | Regroupement des instantanés, décomptes par espace de noms, dépouillement des tâches. Pur, testé. |
| `jobs.rs` | Travaux planifiés et mises à jour en attente en `Sample`. Pur, testé. |
| `view.rs` | La `ProbeView` et le trait `ProbeObserver`. Types purs, sérialisables. |

Tout ce qui est testable l'est sans réseau, à partir d'extraits de réponses en
constantes.
