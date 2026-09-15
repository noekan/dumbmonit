# Collecteur Proxmox VE

Intégration de Proxmox VE : nœuds, machines virtuelles, conteneurs, stockages,
sauvegardes, haute disponibilité, instantanés, réplication, Ceph, mises à jour
et certificats. Le module ne dépend que de `ezymonit-proto`, `reqwest`, `serde`,
`chrono`, `tokio` et `futures`, tous déjà présents, et du client HTTP partagé
`crate::collectors::http`.

## Câblage

Deux lignes à ajouter en dehors de ce répertoire :

* `crates/server/src/collectors/mod.rs`, sous `mod dummy;` :

  ```rust
  mod proxmox;
  ```

  puis, sous `pub use dummy::DummyCollector;` :

  ```rust
  pub use proxmox::ProxmoxCollector;
  ```

* `crates/server/src/main.rs`, sous l'enregistrement du collecteur de démonstration :

  ```rust
  registry.register(Arc::new(collectors::ProxmoxCollector::new()));
  ```

Les cibles de type `proxmox` sont alors interrogées par le planificateur.

## Configuration d'une cible

* **Adresse** : `10.0.0.10`, `pve.lan`, `pve.lan:8006`, `[fd00::1]` ou une URL
  complète `https://pve.example.net`. Le schéma `https` et le port `8006` sont
  ajoutés si besoin.
* **Identifiant** :
  * `Credential::ApiToken` — la chaîne complète affichée par Proxmox à la création
    du jeton : `utilisateur@realm!nom-du-jeton=secret`. **Mode recommandé** : pas
    d'expiration, pas de session ouverte sur l'hyperviseur.
  * `Credential::UsernamePassword` — `utilisateur@realm` et le mot de passe. Un
    ticket est obtenu puis mis en cache et renouvelé dix minutes avant sa fin de
    vie (deux heures).
* **Droits nécessaires** : le rôle `PVEAuditor` sur `/`, en lecture seule. Rien de
  plus n'est requis, le collecteur ne fait que des `GET` — à une exception près :
  `GET /nodes/{node}/apt/update` exige `Sys.Modify` sur `/nodes`. Ce droit est
  facultatif : sans lui, le 403 est journalisé en `debug` et la métrique
  `node_updates_pending` n'est simplement pas publiée.

| Endpoint | Droit |
|---|---|
| `/version`, `/cluster/status`, `/nodes`, `/nodes/{n}/status` | `Sys.Audit` |
| `/nodes/{n}/qemu`, `/nodes/{n}/lxc`, `/nodes/{n}/{qemu,lxc}/{vmid}/snapshot` | `VM.Audit` |
| `/nodes/{n}/storage`, `/nodes/{n}/storage/{s}/content` | `Datastore.Audit` |
| `/nodes/{n}/tasks` | `Sys.Audit` |
| `/cluster/ha/status/current`, `/cluster/backup`, `/cluster/backup-info/not-backed-up`, `/cluster/ceph/status` | `Sys.Audit` |
| `/nodes/{n}/replication`, `/nodes/{n}/certificates/info` | `Sys.Audit` |
| `/nodes/{n}/apt/update` | `Sys.Modify` sur `/nodes` (facultatif) |

### Certificat auto-signé

Une installation Proxmox par défaut présente un certificat auto-signé, que le
collecteur refuse — comme n'importe quel client TLS correct. Le message d'erreur
le dit explicitement et indique la marche à suivre. Deux issues :

1. installer un certificat reconnu sur l'hyperviseur (ACME est intégré à Proxmox) ;
2. ajouter l'étiquette `insecure_tls = true` sur la cible, ce qui désactive toute
   vérification du certificat pour cette cible.

L'option n'est jamais activée implicitement : elle expose la connexion à une
interception, et ce doit rester un choix conscient.

### Étiquettes reconnues

| Étiquette | Défaut | Rôle |
|---|---|---|
| `insecure_tls` | `false` | Accepte un certificat non vérifiable. |
| `port` | `8006` | Port de l'API si l'adresse n'en précise pas. |
| `request_timeout_seconds` | `10` | Délai par requête HTTP (1 à 120). |
| `backup_lookback_days` | `31` | Profondeur d'examen des tâches `vzdump`. |
| `scan_backup_storage` | `true` | Inventorie les archives pour dater les sauvegardes par machine. |
| `nodes` | tous | Liste de nœuds à collecter, séparés par des virgules. |
| `ha` | `true` | État de la haute disponibilité. |
| `backup_jobs` | `true` | Travaux de sauvegarde planifiés et invités non couverts. |
| `scan_snapshots` | `true` | Inventaire des instantanés, un appel par invité, quatre en vol par nœud. |
| `max_snapshot_guests` | `200` | Plafond d'invités inventoriés par collecte (1 à 10000). |
| `replication` | `true` | État de la réplication de stockage. |
| `ceph` | `true` | Santé Ceph ; toute erreur de l'endpoint vaut « pas de Ceph ». |
| `updates` | `true` | Mises à jour en attente (droit facultatif, voir plus haut). |
| `certificates` | `true` | Expiration des certificats de chaque nœud. |

Ces étiquettes sont aussi recopiées en `tag_*` sur les séries, par le registre.

## Organisation

| Fichier | Rôle |
|---|---|
| `mod.rs` | Le collecteur, l'orchestration et la tolérance aux pannes partielles. |
| `client.rs` | Le seul module qui fait du réseau : requêtes, statuts, erreurs. |
| `auth.rs` | En-tête de jeton, cycle de vie du ticket. |
| `options.rs` | Lecture des étiquettes, composition de l'URL de base. |
| `model.rs` | Désérialisation des réponses de l'API. |
| `metrics.rs` | Conversion en `Sample` (cluster, nœuds, invités, stockages, mises à jour, certificats). Pur, donc testé en totalité. |
| `backup.rs` | Datation des sauvegardes, par nœud et par machine ; travaux planifiés et couverture. |
| `ha.rs` | Haute disponibilité : quorum, maître, LRM, ressources. |
| `snapshots.rs` | Nombre et âge des instantanés par invité. |
| `replication.rs` | État des travaux de réplication (`to_node` pour la destination, `target` étant réservé). |
| `ceph.rs` | Santé et capacité Ceph, deux dispositions de réponse tolérées. |

Tout ce qui est testable l'est sans réseau : les modules de conversion partent
d'extraits de réponses en constantes, copiés tels quels du faux
`docker/lab/fakes/pve.py` ; `client.rs` teste la traduction des statuts HTTP en
`ProbeError`, `auth.rs` la validation du jeton et l'expiration du ticket.
