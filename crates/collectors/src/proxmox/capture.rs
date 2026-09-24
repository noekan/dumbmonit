//! Relecture d'une capture réelle de l'API Proxmox VE 9.2.
//!
//! `testdata/pve9/` contient les réponses d'un cluster de production à trois
//! nœuds — sans Ceph, sans conteneur, quelques VM avec et sans agent QEMU —
//! pseudonymisées mais de structure et de types intacts. Le fichier est nommé
//! d'après le chemin interrogé, et les réponses en échec sont enregistrées sous
//! la forme `{"_http_status": …, "_body": "…"}`.
//!
//! Ce module fait passer *le vrai code de désérialisation* sur chacune de ces
//! réponses. L'enjeu n'est pas la couverture : c'est qu'un champ renommé d'une
//! version à l'autre, ou un nombre devenu chaîne, se voie ici en test rouge
//! plutôt que sur un panneau resté vide chez l'utilisateur.

use std::path::{Path, PathBuf};

use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::client::absence_marker;
use super::model::*;
use super::*;

const TS: i64 = 1_758_700_000_000;
const NOW_S: i64 = 1_758_700_000;

fn capture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/proxmox/testdata/pve9")
}

/// Les fichiers de la capture, triés par leur numéro d'ordre.
fn captures() -> Vec<(String, String)> {
    let mut files: Vec<_> = std::fs::read_dir(capture_dir())
        .expect("capture Proxmox lisible")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            Some((name, std::fs::read_to_string(&path).ok()?))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(files.len() > 50, "capture incomplète : {} fichiers", files.len());
    files
}

/// Le statut et le corps d'une réponse enregistrée en échec, s'il y en a une.
fn failure(body: &str) -> Option<(StatusCode, String)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let status = value.get("_http_status")?.as_u64()?;
    Some((
        StatusCode::from_u16(status as u16).expect("statut HTTP valide"),
        value.get("_body")?.as_str().unwrap_or_default().to_string(),
    ))
}

/// Déballe `{"data": …}` comme le fait le client, et échoue avec le nom du
/// fichier pour que le diagnostic n'ait pas besoin d'être cherché.
fn parse<T: DeserializeOwned>(name: &str, body: &str) -> T {
    match serde_json::from_str::<Envelope<T>>(body) {
        Ok(envelope) => envelope.data,
        Err(error) => panic!(
            "{name} : réponse réelle de Proxmox VE 9.2 refusée par {} — {error}",
            std::any::type_name::<T>()
        ),
    }
}

/// Le segment de chemin qu'un nom de fichier encode, sans son numéro d'ordre.
fn endpoint(name: &str) -> &str {
    name.split_once('-').map_or(name, |(_, rest)| rest).trim_end_matches(".json")
}

/// Tout ce que la capture contient se désérialise dans le type que le
/// collecteur emploie pour cet endpoint.
///
/// Le `match` est exhaustif par construction : un endpoint capturé qui ne
/// tombe dans aucune branche fait échouer le test, ce qui oblige à décider
/// consciemment qu'on ne le lit pas.
#[test]
fn toute_la_capture_se_deserialise() {
    let mut lus = 0;
    let mut absents = 0;
    let mut ignores = 0;

    for (name, body) in captures() {
        // Les réponses en échec sont vérifiées ailleurs : ici, on s'assure
        // seulement qu'elles ne sont pas prises pour des données.
        if failure(&body).is_some() {
            absents += 1;
            continue;
        }
        lus += 1;
        let path = endpoint(&name);

        match path {
            "version" => drop(parse::<Version>(&name, &body)),
            "cluster-status" => drop(parse::<Vec<ClusterStatusEntry>>(&name, &body)),
            p if p.starts_with("cluster-resources") => {
                drop(parse::<Vec<ResourceEntry>>(&name, &body))
            }
            "cluster-ha-status-current" => drop(parse::<Vec<HaStatusEntry>>(&name, &body)),
            "cluster-ha-status-manager-status" => drop(parse::<HaManagerStatus>(&name, &body)),
            "cluster-backup" => drop(parse::<Vec<BackupJob>>(&name, &body)),
            p if p.ends_with("included-volumes") => drop(parse::<IncludedVolumes>(&name, &body)),
            "cluster-metrics-export" => drop(parse::<MetricsExport>(&name, &body)),
            "cluster-replication" => drop(parse::<Vec<ReplicationJob>>(&name, &body)),
            "nodes" => drop(parse::<Vec<NodeListEntry>>(&name, &body)),
            p if p.ends_with("-status") => drop(parse::<NodeStatus>(&name, &body)),
            p if p.ends_with("-version") => drop(parse::<Version>(&name, &body)),
            p if p.ends_with("-subscription") => drop(parse::<Subscription>(&name, &body)),
            p if p.ends_with("-services") => drop(parse::<Vec<ServiceEntry>>(&name, &body)),
            p if p.ends_with("-network") => drop(parse::<Vec<NetworkInterface>>(&name, &body)),
            p if p.ends_with("-netstat") => drop(parse::<Vec<NetstatEntry>>(&name, &body)),
            p if p.ends_with("-storage") => drop(parse::<Vec<StorageEntry>>(&name, &body)),
            p if p.ends_with("-disks-list") => drop(parse::<Vec<DiskEntry>>(&name, &body)),
            p if p.ends_with("-disks-zfs") => drop(parse::<Vec<ZfsPool>>(&name, &body)),
            p if p.ends_with("-disks-lvm") => drop(parse::<LvmTree>(&name, &body)),
            p if p.ends_with("-disks-lvmthin") => drop(parse::<Vec<ThinPool>>(&name, &body)),
            p if p.ends_with("-disks-directory") => {
                drop(parse::<Vec<DirectoryMount>>(&name, &body))
            }
            p if p.contains("-disks-smart-") => drop(parse::<SmartReport>(&name, &body)),
            p if p.ends_with("-apt-versions") => drop(parse::<Vec<AptVersion>>(&name, &body)),
            p if p.ends_with("-certificates-info") => {
                drop(parse::<Vec<CertificateInfo>>(&name, &body))
            }
            p if p.ends_with("-replication") => drop(parse::<Vec<ReplicationJob>>(&name, &body)),
            p if p.contains("-tasks") => drop(parse::<Vec<TaskEntry>>(&name, &body)),
            p if p.ends_with("-qemu") || p.ends_with("-lxc") => {
                drop(parse::<Vec<GuestEntry>>(&name, &body))
            }
            p if p.ends_with("-status-current") => drop(parse::<QemuStatus>(&name, &body)),
            p if p.ends_with("-snapshot") => drop(parse::<Vec<Snapshot>>(&name, &body)),
            p if p.ends_with("-agent-get-osinfo") => drop(parse::<AgentOsInfo>(&name, &body)),
            p if p.ends_with("-agent-network-get-interfaces") => {
                drop(parse::<AgentInterfaces>(&name, &body))
            }
            // Endpoints capturés que le collecteur n'interroge pas : ils
            // restent dans la capture pour documenter ce qui existe, et la
            // seule exigence est qu'ils soient du JSON.
            "cluster-options"
            | "cluster-ha-resources"
            | "cluster-config-nodes"
            | "cluster-config-qdevice"
            | "cluster-ceph-metadata" => {
                ignores += 1;
                lus -= 1;
                drop(serde_json::from_str::<serde_json::Value>(&body).expect("JSON valide"));
            }
            p if p.ends_with("-hardware-pci") || p.contains("-rrddata") => {
                ignores += 1;
                lus -= 1;
                drop(serde_json::from_str::<serde_json::Value>(&body).expect("JSON valide"));
            }
            other => panic!("{name} : endpoint capturé sans type associé ({other})"),
        }
    }

    assert!(lus >= 40, "trop peu d'endpoints relus : {lus}");
    assert!(absents >= 10, "les réponses en échec ont disparu de la capture : {absents}");
    assert!(ignores > 0, "les endpoints non consommés ont disparu de la capture");
}

/// Chaque réponse en échec de la capture est classée comme il faut : absence
/// pour Ceph et l'agent QEMU, droit manquant pour `apt/update`.
///
/// C'est la régression qu'on ne veut plus jamais revoir : un cluster sans Ceph
/// ou une VM dont l'agent est arrêté ne sont ni une panne, ni une erreur de
/// collecte.
#[test]
fn les_echecs_de_la_capture_sont_classes_comme_des_absences() {
    let mut ceph = 0;
    let mut agent = 0;
    let mut droits = 0;

    for (name, body) in captures() {
        let Some((status, payload)) = failure(&body) else { continue };
        let path = endpoint(&name);
        let error = super::client::status_error(status, &payload, path);

        if path.contains("ceph") {
            ceph += 1;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{name}");
            assert_eq!(absence_marker(status, &payload), Some("binary not installed"), "{name}");
            assert!(
                !error.means_down(),
                "{name} : Ceph absent ne doit pas déclarer le cluster mort"
            );
        } else if path.contains("agent") {
            agent += 1;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{name}");
            assert_eq!(
                absence_marker(status, &payload),
                Some("guest agent is not running"),
                "{name}"
            );
            assert!(!error.means_down(), "{name} : un agent arrêté n'éteint pas la VM");
        } else if path.contains("apt-update") {
            droits += 1;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
            assert_eq!(absence_marker(status, &payload), None, "{name}");
            assert!(matches!(error, ProbeError::Auth(_)), "{name} : {error}");
            assert!(!error.means_down(), "{name}");
        } else {
            panic!("{name} : échec capturé non classé");
        }
    }

    assert_eq!(ceph, 12, "trois endpoints Ceph de cluster, puis trois par nœud");
    assert_eq!(agent, 6, "trois VM sans agent, deux appels chacune");
    assert_eq!(droits, 3, "apt/update refusé sur les trois nœuds");
}

/// Lit le premier fichier dont le nom contient `motif`.
fn capture<T: DeserializeOwned>(motif: &str) -> T {
    let (name, body) = captures()
        .into_iter()
        .find(|(name, _)| name.contains(motif))
        .unwrap_or_else(|| panic!("capture « {motif} » absente"));
    parse::<T>(&name, &body)
}

/// Toutes les captures réussies dont le nom contient `motif`.
fn toutes<T: DeserializeOwned>(motif: &str) -> Vec<T> {
    toutes_si(|name| name.contains(motif))
}

/// Toutes les captures réussies dont le nom satisfait `retenir`.
fn toutes_si<T: DeserializeOwned>(retenir: impl Fn(&str) -> bool) -> Vec<T> {
    captures()
        .into_iter()
        .filter(|(name, body)| retenir(name) && failure(body).is_none())
        .map(|(name, body)| parse::<T>(&name, &body))
        .collect()
}

/// Les séries que le cluster réel doit produire.
///
/// Un parseur qui accepte la réponse mais n'en tire rien laisse un panneau
/// vide : chaque assertion ici correspond à un panneau de l'interface.
#[test]
fn le_cluster_reel_produit_les_series_attendues() {
    fn noms(samples: &[Sample]) -> BTreeSet<&str> {
        samples.iter().map(|sample| sample.metric.as_str()).collect()
    }
    fn valeur(samples: &[Sample], nom: &str) -> f64 {
        samples
            .iter()
            .find(|sample| sample.metric == nom)
            .unwrap_or_else(|| panic!("série « {nom} » absente"))
            .value
    }

    // Quorum : trois nœuds, cluster quorate.
    let cluster: Vec<ClusterStatusEntry> = capture("cluster-status");
    let samples = metrics::cluster_samples(&cluster, TS);
    assert_eq!(valeur(&samples, "proxmox_cluster_quorate"), 1.0);
    assert_eq!(valeur(&samples, "proxmox_cluster_nodes"), 3.0);

    // Inventaire : 29 VM, aucun conteneur, neuf stockages.
    let ressources: Vec<ResourceEntry> = capture("cluster-resources.json");
    let inventaire = resources::inventory(&ressources, TS);
    assert_eq!(inventaire.guests_by_node.len(), 3, "trois nœuds portent des invités");
    assert!(!inventaire.samples.is_empty());

    // Nœud : charge, mémoire, temps de fonctionnement, versions.
    let node: NodeStatus = capture("nodes-node3-status");
    let samples = metrics::node_samples("node3", &node, TS);
    for attendu in [
        "proxmox_node_cpu_percent",
        "proxmox_node_memory_used_bytes",
        "proxmox_node_memory_total_bytes",
        "proxmox_node_uptime_seconds",
        "proxmox_node_rootfs_percent",
    ] {
        assert!(noms(&samples).contains(attendu), "série « {attendu} » absente du nœud réel");
    }

    // Démons : `pveproxy`, `pvestatd`, `corosync`… tous doivent être vus.
    let services: Vec<ServiceEntry> = capture("nodes-node3-services");
    assert!(!node::service_samples("node3", &services, TS).is_empty());

    // Disques : six NVMe sur node3, six SAS sur node2 — santé et usure.
    let disques: Vec<DiskEntry> = capture("nodes-node3-disks-list");
    assert_eq!(disques.len(), 6);
    let samples = disks::disk_samples("node3", &disques, TS);
    assert!(noms(&samples).contains("proxmox_node_disk_smart_failed"));
    assert!(noms(&samples).contains("proxmox_node_disk_wearout_percent"));

    // ZFS : deux pools par nœud, tous en ligne.
    let pools: Vec<ZfsPool> = capture("nodes-node3-disks-zfs");
    assert_eq!(pools.len(), 2);
    let samples = disks::zfs_samples("node3", &pools, TS);
    assert_eq!(valeur(&samples, "proxmox_node_zfs_pool_degraded"), 0.0);
    assert!(noms(&samples).contains("proxmox_node_zfs_pool_fragmentation_percent"));

    // Certificats : le certificat auto-signé de l'interface et son autorité.
    let certificats: Vec<CertificateInfo> = capture("nodes-node3-certificates-info");
    assert_eq!(certificats.len(), 2);
    let samples = metrics::certificate_samples("node3", &certificats, NOW_S, TS);
    assert!(noms(&samples).contains("proxmox_node_certificate_expiry_days"));

    // Abonnement : aucun, sur les trois nœuds.
    let abonnement: Subscription = capture("nodes-node3-subscription");
    assert_eq!(abonnement.status.as_deref(), Some("notfound"));
    assert!(!apt::subscription_samples("node3", &abonnement, TS).is_empty());

    // Stockages : le local, les pools ZFS et la cible de sauvegarde.
    let stockages: Vec<StorageEntry> = capture("nodes-node3-storage");
    let samples = metrics::storage_samples("node3", &stockages, TS);
    assert!(noms(&samples).contains("proxmox_storage_used_percent"));

    // Invités : l'inventaire du nœud doit donner les mêmes VM que le cluster.
    let invites: Vec<GuestEntry> = capture("nodes-node3-qemu.json");
    assert_eq!(invites.len(), 10);
    let samples = metrics::guest_samples("node3", GuestKind::Qemu, &invites, TS);
    assert!(noms(&samples).contains("proxmox_guest_running"));
    assert!(noms(&samples).contains("proxmox_guest_memory_total_bytes"));

    // Conteneurs : le cluster n'en a aucun — la liste vide ne doit rien casser.
    let conteneurs: Vec<GuestEntry> = capture("nodes-node3-lxc.json");
    assert!(conteneurs.is_empty());
    assert!(metrics::guest_samples("node3", GuestKind::Lxc, &conteneurs, TS).is_empty());

    // Haute disponibilité : quorum et surveillance, sans service géré.
    let ha_entries: Vec<HaStatusEntry> = capture("cluster-ha-status-current");
    let samples = ha::ha_samples(&ha_entries, TS);
    assert_eq!(valeur(&samples, "proxmox_ha_quorum_ok"), 1.0);
    assert_eq!(valeur(&samples, "proxmox_ha_resources_total"), 0.0);

    // Travaux de sauvegarde planifiés : un seul, qui couvre des VM nommées.
    let travaux: Vec<BackupJob> = capture("cluster-backup.json");
    assert_eq!(travaux.len(), 1);
    let volumes: IncludedVolumes = capture("included-volumes");
    assert!(!backup::included_volume_samples(&travaux[0].id, &volumes, TS).is_empty());

    // Historique des tâches : sauvegardes et démarrages des derniers jours.
    let taches: Vec<TaskEntry> = capture("nodes-node3-tasks");
    assert!(!backup::job_samples("node3", &taches, NOW_S, 90 * 86_400, TS).is_empty());

    // Réseau : ponts, liens physiques et compteurs par invité.
    let interfaces: Vec<NetworkInterface> = capture("nodes-node3-network");
    assert!(!node::network_samples("node3", &interfaces, TS).is_empty());
    let netstat: Vec<NetstatEntry> = capture("nodes-node3-netstat");
    assert!(!node::netstat_samples("node3", &netstat, TS).is_empty());

    // LVM : le cluster relevé n'en a aucun (`{"leaf":0,"children":[]}`), et la
    // réponse vide ne doit rien publier plutôt que des groupes à zéro.
    let lvm: LvmTree = capture("nodes-node3-disks-lvm");
    assert!(node::lvm_samples("node3", &lvm, TS).is_empty());

    // Paquets : la liste des versions installées alimente le suivi des
    // changements, même sans `Sys.Modify` pour compter les mises à jour.
    let versions: Vec<AptVersion> = capture("nodes-node3-apt-versions");
    assert!(versions.len() > 20, "{} paquets", versions.len());
    assert!(versions.iter().any(|package| package.package.as_deref() == Some("proxmox-ve")));

    // Flux RRD complet : tous les nœuds et toutes les VM y passent.
    let export: MetricsExport = capture("cluster-metrics-export");
    let flux = export::export_samples(&export, None);
    assert!(flux.samples.len() > 100, "{} points", flux.samples.len());
}

/// L'état détaillé d'une VM en marche, tel que Proxmox VE 9 le rend.
#[test]
fn letat_detaille_des_vm_reelles_est_exploitable() {
    let statuts: Vec<QemuStatus> =
        toutes_si(|name| name.contains("-qemu-") && name.ends_with("status-current.json"));
    assert!(statuts.len() >= 10, "{} VM capturées", statuts.len());

    let entry = GuestEntry {
        vmid: Num(101.0),
        name: Some("WEB01".to_string()),
        status: Some("running".to_string()),
        ..Default::default()
    };

    let mut avec_ballon = 0;
    for status in &statuts {
        let samples = guest::qemu_status_samples("node3", &entry, status, TS);
        assert!(
            samples.iter().any(|sample| sample.metric == "proxmox_guest_agent_enabled"),
            "l'activation de l'agent doit toujours être publiée"
        );
        if samples.iter().any(|sample| sample.metric == "proxmox_guest_balloon_bytes") {
            avec_ballon += 1;
        }
    }
    assert!(avec_ballon >= 6, "{avec_ballon} VM avec ballon");
}

/// Le système et les adresses lus par l'agent QEMU.
#[test]
fn lagent_qemu_reel_donne_systeme_et_adresses() {
    let entry = GuestEntry { vmid: Num(101.0), ..Default::default() };

    let systemes: Vec<AgentOsInfo> = toutes("agent-get-osinfo");
    assert!(systemes.len() >= 3);
    for info in &systemes {
        assert!(
            !guest::os_samples("node3", &entry, info, TS).is_empty(),
            "un agent qui répond doit toujours donner de quoi nommer le système"
        );
    }

    let reseaux: Vec<AgentInterfaces> = toutes("agent-network-get-interfaces");
    assert!(reseaux.len() >= 3);
    assert!(
        reseaux
            .iter()
            .any(|list| !guest::agent_address_samples("node3", &entry, list, TS).is_empty()),
        "au moins une VM doit publier une adresse"
    );
}

/// Les rapports SMART du cluster : NVMe et SAS, tous deux en texte libre.
///
/// Proxmox VE 9 ne rend plus `attributes` que pour l'ATA classique ; les six
/// NVMe de `node3` et les six disques SAS de `node2` répondent
/// `type: "text"`, avec deux formulations différentes de la température.
#[test]
fn les_rapports_smart_reels_donnent_la_temperature() {
    let rapports: Vec<SmartReport> = toutes("disks-smart");
    assert!(rapports.len() >= 2, "{} rapports", rapports.len());
    for rapport in &rapports {
        assert!(
            rapport.temperature().is_some(),
            "température illisible dans un rapport réel : {:?}",
            rapport.text.as_deref().map(|text| &text[..text.len().min(120)])
        );
    }
}

/// Le taux d'occupation mémoire d'une VM ne dépasse jamais 100 %.
///
/// Proxmox VE 9 rapporte dans les inventaires la mémoire vue de l'hôte,
/// surcoût d'émulation compris : sur ce cluster, 26 VM sur 29 dépassaient
/// `maxmem`. Publiée telle quelle, la série faisait crier en permanence la
/// règle « mémoire presque pleine » (> 95 % pendant dix minutes) sur presque
/// tout le parc.
#[test]
fn la_memoire_des_vm_reelles_ne_depasse_jamais_cent_pour_cent() {
    let mut vus = 0;
    for (name, body) in captures() {
        if failure(&body).is_some() {
            continue;
        }
        let path = endpoint(&name);
        let samples = if path.ends_with("-qemu") {
            let guests: Vec<GuestEntry> = parse(&name, &body);
            metrics::guest_samples("node3", GuestKind::Qemu, &guests, TS)
        } else if path == "cluster-resources" {
            let entries: Vec<ResourceEntry> = parse(&name, &body);
            resources::inventory(&entries, TS).samples
        } else if path.contains("-qemu-") && path.ends_with("-status-current") {
            let status: QemuStatus = parse(&name, &body);
            let entry = GuestEntry { vmid: Num(1.0), ..Default::default() };
            guest::qemu_status_samples("node3", &entry, &status, TS)
        } else {
            continue;
        };

        for sample in samples.iter().filter(|s| s.metric == "proxmox_guest_memory_percent") {
            vus += 1;
            assert!(
                sample.value <= 100.0,
                "{name} : {} % de mémoire, la règle « mémoire presque pleine » crierait à vide",
                sample.value
            );
        }
    }
    assert!(vus >= 20, "seulement {vus} taux de mémoire publiés");
}

/// Ce que l'état du nœud donne en plus depuis Proxmox VE 9 : l'attente disque
/// et la mémoire rendue par la déduplication de pages.
#[test]
fn le_noeud_reel_publie_lattente_disque() {
    let status: NodeStatus = capture("nodes-node2-status");
    let samples = metrics::node_samples("node2", &status, TS);
    let attente = samples
        .iter()
        .find(|sample| sample.metric == "proxmox_node_cpu_iowait_percent")
        .expect("l'attente disque est dans la réponse réelle");
    assert!(attente.value > 0.0 && attente.value < 100.0, "{}", attente.value);
}

/// Sans `Sys.Modify`, `apt/update` est refusé sur les trois nœuds : la liste
/// des versions reste la seule à pouvoir dire qu'une mise à jour attend, et
/// qu'un noyau plus récent est installé sans avoir été démarré.
#[test]
fn les_versions_de_paquets_reelles_disent_le_redemarrage_en_attente() {
    let versions: Vec<AptVersion> = capture("nodes-node3-apt-versions");
    let samples = apt::version_samples("node3", &versions, TS);

    let attente = samples
        .iter()
        .find(|sample| sample.metric == "proxmox_node_pve_packages_upgradable")
        .expect("série des paquets à mettre à jour");
    assert!(attente.value > 0.0, "le cluster réel a des mises à jour en attente");

    let reboot = samples
        .iter()
        .find(|sample| sample.metric == "proxmox_node_reboot_required")
        .expect("série de redémarrage en attente");
    // Le nœud tourne sur 7.0.6-2-pve alors que 7.0.14-11 est installé.
    assert_eq!(reboot.value, 1.0);
    assert_eq!(reboot.labels.get("running").map(String::as_str), Some("7.0.6-2-pve"));
    assert_eq!(reboot.labels.get("installed").map(String::as_str), Some("7.0.14-11"));
}

/// Les deux volumes que le cluster réel porte en `backup=no` sont bien
/// signalés, un par invité.
///
/// C'est le cœur de la règle « un travail de sauvegarde saute un disque » : une
/// sauvegarde qui réussit tous les soirs sans emporter le second disque d'un
/// contrôleur de domaine ne se découvre qu'au jour de la restauration. Seules
/// les exclusions structurelles (lecteur de CD, `cloudinit`) restent muettes.
#[test]
fn un_volume_ecarte_a_la_main_est_signale() {
    let tree: IncludedVolumes = capture("included-volumes");
    let raisons: Vec<&str> = tree
        .children
        .iter()
        .flat_map(|guest| &guest.children)
        .filter(|volume| !volume.is_included())
        .filter_map(|volume| volume.reason.as_deref())
        .collect();
    assert!(raisons.contains(&"backup=no"), "la capture doit contenir le cas réel : {raisons:?}");

    let samples = backup::included_volume_samples("backup-job", &tree, TS);
    let signales: Vec<&Sample> = samples
        .iter()
        .filter(|sample| sample.metric == "proxmox_backup_job_guest_excluded_volumes")
        .collect();
    assert_eq!(signales.len(), 2, "un signalement par invité concerné");
    for sample in signales {
        assert_eq!(sample.labels.get("reason").map(String::as_str), Some("backup=no"));
    }
    assert!(samples.iter().any(|s| s.metric == "proxmox_backup_job_volumes_excluded"));
}

/// La ligne `fencing` de Proxmox VE 9 : sans ressource confiée à la HA, le
/// chien de garde reste en attente, et rien ne sera isolé.
#[test]
fn letat_du_chien_de_garde_est_publie() {
    let entries: Vec<HaStatusEntry> = capture("cluster-ha-status-current");
    let samples = ha::ha_samples(&entries, TS);
    let fencing = samples
        .iter()
        .find(|sample| sample.metric == "proxmox_ha_fencing_armed")
        .expect("la ligne fencing du cluster réel");
    assert_eq!(fencing.value, 0.0);
    assert_eq!(fencing.labels.get("state").map(String::as_str), Some("standby"));
}

/// Le datastore PBS du cluster est actif mais n'annonce aucune capacité : PVE
/// ne la lit pas. Trois jauges à zéro octet donneraient à lire une cible de
/// sauvegarde vide, ce qui est pire que pas de mesure du tout.
#[test]
fn un_stockage_sans_capacite_ne_publie_pas_de_zero() {
    let storages: Vec<StorageEntry> = capture("nodes-node3-storage");
    let sans_capacite: Vec<&str> = storages
        .iter()
        .filter(|storage| storage.is_active() && storage.total.is_some_and(|t| t.0 == 0.0))
        .map(|storage| storage.storage.as_str())
        .collect();
    assert!(!sans_capacite.is_empty(), "la capture doit contenir le cas réel");

    let samples = metrics::storage_samples("node3", &storages, TS);
    for nom in sans_capacite {
        for sample in &samples {
            if sample.labels.get("storage").map(String::as_str) != Some(nom) {
                continue;
            }
            assert!(
                !sample.metric.ends_with("_bytes") && !sample.metric.ends_with("_percent"),
                "{nom} : {} publiée à {}",
                sample.metric,
                sample.value
            );
        }
    }
}
