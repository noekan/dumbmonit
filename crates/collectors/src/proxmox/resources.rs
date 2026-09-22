//! Inventaire du cluster : `GET /cluster/resources`.
//!
//! Un seul appel décrit tout ce que le cluster connaît — nœuds, machines
//! virtuelles, conteneurs, stockages et pools — avec leur état et leurs mesures.
//! C'est la colonne vertébrale de la collecte, pour deux raisons.
//!
//! * **Rien ne manque.** La tournée par nœud interroge `/nodes/{n}/qemu` et
//!   `/nodes/{n}/lxc` : un nœud injoignable emporte avec lui la liste de ses
//!   invités, qui disparaissent alors de l'inventaire — au moment précis où l'on
//!   voudrait les voir. `/cluster/resources` répond depuis le cache du cluster
//!   (`pmxcfs`), donc les liste quand même, avec `status = unknown`.
//! * **C'est moins cher.** Deux appels par nœud remplacés par un seul, quel que
//!   soit le nombre de nœuds.
//!
//! Ce que l'endpoint ne porte pas reste collecté ailleurs : l'état QEMU détaillé
//! (`status/current`, ballon), les systèmes de fichiers vus par l'agent, les
//! instantanés, les disques physiques. La tournée des nœuds garde donc toute sa
//! raison d'être ; elle perd seulement l'inventaire des invités.

use std::collections::BTreeMap;

use dumbmonit_proto::Sample;

use super::metrics::{self, GuestKind};
use super::model::ResourceEntry;

/// Ce que l'inventaire du cluster livre : des séries, et de quoi savoir quels
/// invités ont déjà été comptés.
#[derive(Default)]
pub struct Inventory {
    pub samples: Vec<Sample>,
    /// Invités du cluster, par nœud : `(vmid, nom, type, en marche)`.
    pub guests_by_node: BTreeMap<String, Vec<GuestSummary>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuestSummary {
    pub vmid: i64,
    pub name: String,
    pub kind: GuestKind,
    pub running: bool,
    pub template: bool,
}

/// Traduit `/cluster/resources` en séries et en inventaire d'invités.
pub fn inventory(entries: &[ResourceEntry], ts_ms: i64) -> Inventory {
    let mut inventory = Inventory::default();
    let mut guests = 0u32;
    let mut running = 0u32;
    let mut templates = 0u32;
    let mut pools: BTreeMap<String, u32> = BTreeMap::new();

    for entry in entries {
        let Some(kind) = entry.guest_kind() else { continue };
        let Some(vmid) = entry.vmid() else { continue };
        let kind = if kind == "qemu" { GuestKind::Qemu } else { GuestKind::Lxc };
        // Un invité sans nœud est un invité dont le cluster a perdu la trace :
        // il n'y a rien à en dire de fiable, et aucune étiquette à lui donner.
        let Some(node) = entry.node.clone().filter(|node| !node.is_empty()) else { continue };

        let guest = entry.to_guest_entry();
        inventory.samples.extend(metrics::guest_samples(
            &node,
            kind,
            std::slice::from_ref(&guest),
            ts_ms,
        ));

        let template = guest.is_template();
        if template {
            templates += 1;
        } else {
            guests += 1;
            if guest.is_running() {
                running += 1;
            }
        }

        let identity = |sample: Sample| metrics::guest_labels(sample, &node, &guest, kind);

        // Le pool est une notion propre au cluster : il ne figure sur aucune des
        // listes par nœud, et c'est pourtant la façon dont beaucoup de parcs sont
        // découpés (par client, par service).
        if let Some(pool) = entry.pool.as_deref().filter(|pool| !pool.is_empty()) {
            inventory.samples.push(identity(
                metrics::gauge("guest_pool_info", 1.0, ts_ms).with_label("pool", pool),
            ));
            *pools.entry(pool.to_string()).or_default() += 1;
        }

        // Un verrou est normal pendant une sauvegarde ou une migration ; laissé
        // en place, il empêche toute opération sur la machine. La série n'existe
        // que tant que le verrou est posé, ce qui rend sa durée lisible telle
        // quelle.
        if let Some(lock) = entry.lock.as_deref().filter(|lock| !lock.is_empty()) {
            inventory.samples.push(identity(
                metrics::gauge("guest_locked", 1.0, ts_ms).with_label("lock", lock),
            ));
        }

        inventory.guests_by_node.entry(node).or_default().push(GuestSummary {
            vmid,
            name: guest.display_name(),
            kind,
            running: guest.is_running(),
            template,
        });
    }

    for (pool, count) in pools {
        inventory
            .samples
            .push(metrics::gauge("pool_guests", f64::from(count), ts_ms).with_label("pool", pool));
    }

    // Publiés même à zéro : ce sont eux qui distinguent « cluster vide » de
    // « inventaire non collecté ».
    inventory.samples.push(metrics::gauge("cluster_guests_total", f64::from(guests), ts_ms));
    inventory.samples.push(metrics::gauge("cluster_guests_running", f64::from(running), ts_ms));
    inventory.samples.push(metrics::gauge("cluster_templates_total", f64::from(templates), ts_ms));

    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// `GET /cluster/resources` d'un cluster de deux nœuds dont le second est
    /// injoignable : ses invités restent listés, sans mesure, en `unknown`.
    const RESOURCES: &str = r#"{"data":[
      {"type":"node","id":"node/pve1","node":"pve1","status":"online","cpu":0.042,"maxcpu":8,"mem":41231686656,"maxmem":68719476736,"uptime":1699284,"level":""},
      {"type":"node","id":"node/pve2","node":"pve2","status":"unknown","maxcpu":4,"maxmem":34359738368},
      {"type":"storage","id":"storage/pve1/local","node":"pve1","storage":"local","plugintype":"dir","status":"available","content":"iso,backup","shared":0,"disk":21463228416,"maxdisk":100861726720},
      {"type":"pool","id":"pool/production","pool":"production"},
      {"type":"qemu","id":"qemu/100","node":"pve1","vmid":100,"name":"router-vm","status":"running","cpu":0.05,"maxcpu":2,"mem":1398101333,"maxmem":2147483648,"disk":0,"maxdisk":17179869184,"netin":9876543210,"netout":1234567890,"diskread":54321,"diskwrite":12345,"uptime":864000,"pool":"production","hastate":"started","tags":"prod"},
      {"type":"qemu","id":"qemu/9000","node":"pve1","vmid":9000,"name":"debian-12-template","status":"stopped","template":1,"maxcpu":2,"maxmem":2147483648,"maxdisk":34359738368},
      {"type":"lxc","id":"lxc/202","node":"pve2","vmid":202,"name":"nextcloud","status":"unknown","maxcpu":4,"maxmem":4294967296,"maxdisk":107374182400,"pool":"production","lock":"backup"},
      {"type":"openvz","id":"openvz/300","node":"pve1","vmid":300,"name":"ancien","status":"running","maxcpu":1,"maxmem":536870912},
      {"type":"sdn","id":"sdn/zone1","status":"available"}
    ]}"#;

    fn extraire(json: &str) -> Vec<ResourceEntry> {
        serde_json::from_str::<Envelope<Vec<ResourceEntry>>>(json).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn linventaire_voit_les_invites_dun_noeud_injoignable() {
        let inventory = inventory(&extraire(RESOURCES), 1000);

        let sur_pve2 = &inventory.guests_by_node["pve2"];
        assert_eq!(sur_pve2.len(), 1);
        assert_eq!(sur_pve2[0].vmid, 202);
        assert_eq!(sur_pve2[0].name, "nextcloud");
        assert!(!sur_pve2[0].running, "« unknown » n'est pas « running »");

        // La taille reste connue même sans mesure : c'est le cache du cluster.
        assert_eq!(
            valeur(
                &inventory.samples,
                r#"proxmox_guest_memory_total_bytes{name="nextcloud",node="pve2",type="lxc",vmid="202"}"#
            ),
            Some(4294967296.0)
        );
        assert_eq!(
            valeur(
                &inventory.samples,
                r#"proxmox_guest_running{name="nextcloud",node="pve2",type="lxc",vmid="202"}"#
            ),
            Some(0.0)
        );
    }

    #[test]
    fn les_mesures_dun_invite_en_marche_sont_reprises_telles_quelles() {
        let inventory = inventory(&extraire(RESOURCES), 1000);
        let vm = r#"{name="router-vm",node="pve1",type="qemu",vmid="100"}"#;
        assert_eq!(valeur(&inventory.samples, &format!("proxmox_guest_running{vm}")), Some(1.0));
        let cpu = valeur(&inventory.samples, &format!("proxmox_guest_cpu_percent{vm}")).unwrap();
        assert!((cpu - 5.0).abs() < 0.001, "processeur à {cpu} %");
        assert_eq!(
            valeur(&inventory.samples, &format!("proxmox_guest_uptime_seconds{vm}")),
            Some(864000.0)
        );
    }

    #[test]
    fn les_pools_et_les_verrous_sont_publies() {
        let inventory = inventory(&extraire(RESOURCES), 1000);
        assert_eq!(
            valeur(
                &inventory.samples,
                r#"proxmox_guest_pool_info{name="router-vm",node="pve1",pool="production",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );
        assert_eq!(
            valeur(&inventory.samples, r#"proxmox_pool_guests{pool="production"}"#),
            Some(2.0)
        );
        assert_eq!(
            valeur(
                &inventory.samples,
                r#"proxmox_guest_locked{lock="backup",name="nextcloud",node="pve2",type="lxc",vmid="202"}"#
            ),
            Some(1.0)
        );
        // Pas de verrou posé : pas de série du tout, pour que la durée du verrou
        // se lise directement sur le graphe.
        assert!(
            !inventory
                .samples
                .iter()
                .any(|s| s.metric == "proxmox_guest_locked" && s.labels["vmid"] == "100")
        );
    }

    #[test]
    fn les_totaux_distinguent_invites_et_modeles() {
        let inventory = inventory(&extraire(RESOURCES), 1000);
        assert_eq!(valeur(&inventory.samples, "proxmox_cluster_guests_total"), Some(3.0));
        assert_eq!(valeur(&inventory.samples, "proxmox_cluster_guests_running"), Some(2.0));
        assert_eq!(valeur(&inventory.samples, "proxmox_cluster_templates_total"), Some(1.0));
    }

    #[test]
    fn un_conteneur_openvz_est_traite_comme_du_lxc() {
        let inventory = inventory(&extraire(RESOURCES), 1000);
        let ancien = inventory.guests_by_node["pve1"].iter().find(|g| g.vmid == 300).unwrap();
        assert_eq!(ancien.kind, GuestKind::Lxc);
    }

    #[test]
    fn les_entrees_qui_ne_sont_pas_des_invites_sont_ignorees() {
        let inventory = inventory(&extraire(RESOURCES), 1000);
        let vmids: Vec<i64> =
            inventory.guests_by_node.values().flatten().map(|guest| guest.vmid).collect();
        assert_eq!(vmids, vec![100, 9000, 300, 202]);
    }

    #[test]
    fn un_cluster_vide_publie_quand_meme_ses_totaux() {
        let inventory = inventory(&[], 1000);
        assert_eq!(valeur(&inventory.samples, "proxmox_cluster_guests_total"), Some(0.0));
        assert!(inventory.guests_by_node.is_empty());
    }
}
