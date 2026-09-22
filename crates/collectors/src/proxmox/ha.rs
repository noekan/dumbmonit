//! Haute disponibilité : `GET /cluster/ha/status/current`.
//!
//! Le tableau mêle quatre natures d'entrées. Les trois premières décrivent le
//! gestionnaire lui-même (quorum, maître CRM, un LRM par nœud) ; la quatrième,
//! une par ressource, porte l'état qui intéresse vraiment : une machine sous HA
//! passée en `error` ou en `fence` ne redémarrera pas toute seule.

use dumbmonit_proto::Sample;

use super::metrics::{GuestKind, gauge};
use super::model::{HaManagerStatus, HaStatusEntry};

/// États dans lesquels le gestionnaire a renoncé à relancer la ressource.
const ERROR_STATES: [&str; 3] = ["error", "fence", "recovery"];

/// Type et VMID d'une ressource, tirés de son identifiant `vm:100` / `ct:200`.
fn parse_sid(sid: &str) -> Option<(GuestKind, &str)> {
    let (prefix, vmid) = sid.split_once(':')?;
    let kind = match prefix {
        "vm" => GuestKind::Qemu,
        "ct" => GuestKind::Lxc,
        _ => return None,
    };
    Some((kind, vmid))
}

pub fn ha_samples(entries: &[HaStatusEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    if let Some(quorum) = entries.iter().find(|entry| entry.is("quorum")) {
        // `quorate` est le champ fiable ; `status` vaut `OK` sur les versions qui
        // ne le renvoient pas.
        let ok = match quorum.quorate {
            Some(flag) => flag.0 != 0.0,
            None => quorum.status.as_deref() == Some("OK"),
        };
        samples.push(gauge("ha_quorum_ok", if ok { 1.0 } else { 0.0 }, ts_ms));
    }

    if let Some(master) = entries.iter().find(|entry| entry.is("master")) {
        let active = master.status.as_deref() == Some("active");
        samples.push(gauge("ha_master_active", if active { 1.0 } else { 0.0 }, ts_ms));
    }

    for lrm in entries.iter().filter(|entry| entry.is("lrm")) {
        let Some(node) = lrm.node.clone() else { continue };
        let active = lrm.status.as_deref() == Some("active");
        samples.push(
            gauge("ha_lrm_active", if active { 1.0 } else { 0.0 }, ts_ms).with_label("node", node),
        );
    }

    let mut resources = 0u32;
    for service in entries.iter().filter(|entry| entry.is("service")) {
        let Some(sid) = service.sid.as_deref() else { continue };
        let Some((kind, vmid)) = parse_sid(sid) else { continue };
        resources += 1;

        let state = service.service_state().to_string();
        let node = service.node.clone().unwrap_or_default();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("sid", sid)
                    .with_label("node", node.clone())
                    .with_label("vmid", vmid)
                    .with_label("type", kind.as_str()),
            );
        };

        push(gauge("ha_resource_started", if state == "started" { 1.0 } else { 0.0 }, ts_ms));
        let in_error = ERROR_STATES.contains(&state.as_str());
        push(gauge("ha_resource_error", if in_error { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("ha_resource_state_info", 1.0, ts_ms).with_label("state", state.clone()));
    }

    // Toujours publié, même à zéro : c'est ce qui distingue « HA sans ressource »
    // de « HA non collectée ».
    samples.push(gauge("ha_resources_total", f64::from(resources), ts_ms));
    samples
}

/// Délai au-delà duquel l'horodatage d'un LRM est considéré périmé.
///
/// Le gestionnaire réécrit son état toutes les dix secondes ; un nœud dont
/// l'horodatage a deux minutes ne surveille plus rien, et ses machines HA ne
/// seront ni relancées ni déplacées.
const LRM_STALE_SECONDS: f64 = 120.0;

/// `GET /cluster/ha/status/manager_status`.
///
/// `status/current` donne une liste de lignes déjà mises en forme pour
/// l'interface ; `manager_status` donne la structure elle-même, et deux choses
/// qu'on ne trouve nulle part ailleurs : l'avis du gestionnaire sur chaque nœud
/// (`online`, `fence`, `gone`, `maintenance`) et l'horodatage de chaque LRM,
/// qui dit si l'agent local répond encore.
pub fn manager_samples(status: &HaManagerStatus, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    if let Some(core) = &status.manager_status {
        if let Some(master) = core.master_node.as_deref().filter(|node| !node.is_empty()) {
            samples.push(gauge("ha_master_info", 1.0, ts_ms).with_label("node", master));
        }
        if let Some(timestamp) = core.timestamp {
            samples.push(gauge(
                "ha_manager_age_seconds",
                (now_s as f64 - timestamp.0).max(0.0),
                ts_ms,
            ));
        }
        for (node, state) in &core.node_status {
            samples.push(
                gauge("ha_node_online", if state == "online" { 1.0 } else { 0.0 }, ts_ms)
                    .with_label("node", node.clone()),
            );
            samples.push(
                gauge("ha_node_status_info", 1.0, ts_ms)
                    .with_label("node", node.clone())
                    .with_label("status", state.clone()),
            );
        }
    }

    for (node, lrm) in &status.lrm_status {
        let mut push = |sample: Sample| samples.push(sample.with_label("node", node.clone()));
        push(
            gauge("ha_lrm_mode_info", 1.0, ts_ms)
                .with_label("mode", lrm.mode.clone().unwrap_or_default())
                .with_label("state", lrm.state.clone().unwrap_or_default()),
        );
        // Sans horodatage, on ne conclut rien : le LRM d'un nœud qui vient de
        // rejoindre le cluster n'a pas encore écrit.
        if let Some(timestamp) = lrm.timestamp {
            let age = (now_s as f64 - timestamp.0).max(0.0);
            push(gauge("ha_lrm_age_seconds", age, ts_ms));
            push(gauge("ha_lrm_stale", if age > LRM_STALE_SECONDS { 1.0 } else { 0.0 }, ts_ms));
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// Réponse du faux `LAB_SCENARIO=ha-error` : vm:100 en erreur, ct:202 démarré.
    const HA_STATUS: &str = r#"{"data":[
      {"id":"quorum","type":"quorum","node":"pve1","status":"OK","quorate":1},
      {"id":"master","type":"master","node":"pve1","status":"active","timestamp":1789510633},
      {"id":"lrm:pve1","type":"lrm","node":"pve1","status":"active","timestamp":1789510634},
      {"id":"lrm:pve2","type":"lrm","node":"pve2","status":"active","timestamp":1789510634},
      {"id":"service:vm:100","type":"service","sid":"vm:100","node":"pve1","status":"error","state":"error","request_state":"started","crm_state":"error","max_relocate":1,"max_restart":1,"group":"prod"},
      {"id":"service:ct:202","type":"service","sid":"ct:202","node":"pve2","status":"started","state":"started","request_state":"started","crm_state":"started","max_relocate":1,"max_restart":1}
    ]}"#;

    /// Cluster sans ressource HA et sans quorum, LRM de pve2 muet.
    const HA_NO_RESOURCES: &str = r#"{"data":[
      {"id":"quorum","type":"quorum","node":"pve1","status":"No quorum on node 'pve1'!","quorate":0},
      {"id":"master","type":"master","node":"pve1","status":"idle","timestamp":1789510633},
      {"id":"lrm:pve1","type":"lrm","node":"pve1","status":"idle","timestamp":1789510634},
      {"id":"lrm:pve2","type":"lrm","node":"pve2","status":"old timestamp - dead?","timestamp":1789510034}
    ]}"#;

    fn extraire(json: &str) -> Vec<HaStatusEntry> {
        serde_json::from_str::<Envelope<Vec<HaStatusEntry>>>(json).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn une_ressource_en_erreur_est_signalee_avec_son_identite() {
        let samples = ha_samples(&extraire(HA_STATUS), 1000);

        assert_eq!(valeur(&samples, "proxmox_ha_quorum_ok"), Some(1.0));
        assert_eq!(valeur(&samples, "proxmox_ha_master_active"), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_lrm_active{node="pve2"}"#), Some(1.0));
        assert_eq!(valeur(&samples, "proxmox_ha_resources_total"), Some(2.0));

        let vm = r#"{node="pve1",sid="vm:100",type="qemu",vmid="100"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_ha_resource_started{vm}")), Some(0.0));
        assert_eq!(valeur(&samples, &format!("proxmox_ha_resource_error{vm}")), Some(1.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_ha_resource_state_info{node="pve1",sid="vm:100",state="error",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );

        let ct = r#"{node="pve2",sid="ct:202",type="lxc",vmid="202"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_ha_resource_started{ct}")), Some(1.0));
        assert_eq!(valeur(&samples, &format!("proxmox_ha_resource_error{ct}")), Some(0.0));
    }

    #[test]
    fn sans_ressource_seul_le_total_a_zero_est_publie() {
        let samples = ha_samples(&extraire(HA_NO_RESOURCES), 1000);

        assert_eq!(valeur(&samples, "proxmox_ha_resources_total"), Some(0.0));
        assert!(samples.iter().all(|s| !s.metric.starts_with("proxmox_ha_resource_")));
        assert_eq!(valeur(&samples, "proxmox_ha_quorum_ok"), Some(0.0));
        assert_eq!(valeur(&samples, "proxmox_ha_master_active"), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_lrm_active{node="pve2"}"#), Some(0.0));
    }

    #[test]
    fn les_etats_fence_et_recovery_comptent_comme_des_erreurs() {
        for etat in ["fence", "recovery", "error"] {
            let json = format!(
                r#"{{"data":[{{"id":"service:vm:1","type":"service","sid":"vm:1","node":"pve1","status":"{etat}","state":"{etat}"}}]}}"#
            );
            let samples = ha_samples(&extraire(&json), 1000);
            assert_eq!(
                valeur(
                    &samples,
                    r#"proxmox_ha_resource_error{node="pve1",sid="vm:1",type="qemu",vmid="1"}"#
                ),
                Some(1.0),
                "« {etat} » doit être une erreur"
            );
        }
        let samples = ha_samples(
            &extraire(
                r#"{"data":[{"id":"service:ct:2","type":"service","sid":"ct:2","node":"pve1","status":"stopped","state":"stopped"}]}"#,
            ),
            1000,
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_ha_resource_error{node="pve1",sid="ct:2",type="lxc",vmid="2"}"#
            ),
            Some(0.0)
        );
    }

    #[test]
    fn un_identifiant_de_service_inconnu_est_ignore() {
        assert_eq!(parse_sid("vm:100"), Some((GuestKind::Qemu, "100")));
        assert_eq!(parse_sid("ct:200"), Some((GuestKind::Lxc, "200")));
        assert_eq!(parse_sid("bizarre"), None);
        assert_eq!(parse_sid("fs:1"), None);
    }

    /// `GET /cluster/ha/status/manager_status` d'un cluster dont pve2 est parti :
    /// le gestionnaire le voit « unknown » et son LRM n'écrit plus.
    const MANAGER_STATUS: &str = r#"{"data":{
      "quorum":{"node":"pve1","quorate":"1"},
      "manager_status":{"master_node":"pve1","timestamp":1789510630,
        "node_status":{"pve1":"online","pve2":"unknown"},
        "service_status":{"vm:100":{"node":"pve1","state":"started","uid":"abc"}}},
      "lrm_status":{
        "pve1":{"mode":"active","state":"wait_for_agent_lock","timestamp":1789510634,"results":{}},
        "pve2":{"mode":"active","state":"active","timestamp":1789510000,"results":{}}}
    }}"#;

    #[test]
    fn le_gestionnaire_designe_son_maitre_et_juge_chaque_noeud() {
        let status =
            serde_json::from_str::<Envelope<HaManagerStatus>>(MANAGER_STATUS).unwrap().data;
        let samples = manager_samples(&status, 1_789_510_640, 1000);

        assert_eq!(valeur(&samples, r#"proxmox_ha_master_info{node="pve1"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_node_online{node="pve1"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_node_online{node="pve2"}"#), Some(0.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_ha_node_status_info{node="pve2",status="unknown"}"#),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_ha_manager_age_seconds"), Some(10.0));
    }

    #[test]
    fn un_lrm_qui_necrit_plus_est_signale() {
        let status =
            serde_json::from_str::<Envelope<HaManagerStatus>>(MANAGER_STATUS).unwrap().data;
        let samples = manager_samples(&status, 1_789_510_640, 1000);

        assert_eq!(valeur(&samples, r#"proxmox_ha_lrm_stale{node="pve1"}"#), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_lrm_stale{node="pve2"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ha_lrm_age_seconds{node="pve2"}"#), Some(640.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_ha_lrm_mode_info{mode="active",node="pve1",state="wait_for_agent_lock"}"#
            ),
            Some(1.0)
        );
    }

    #[test]
    fn une_reponse_vide_ne_produit_rien_et_ne_panique_pas() {
        let status =
            serde_json::from_str::<Envelope<HaManagerStatus>>(r#"{"data":{}}"#).unwrap().data;
        assert!(manager_samples(&status, 1_000, 1000).is_empty());
    }
}
