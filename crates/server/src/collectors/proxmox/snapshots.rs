//! Instantanés : `GET /nodes/{node}/qemu/{vmid}/snapshot` et `.../lxc/{vmid}/snapshot`.
//!
//! Un instantané oublié est le fuite de disque la plus banale d'un hyperviseur :
//! la chaîne de deltas grossit à chaque écriture, et rien dans l'interface ne le
//! rappelle. On publie donc leur nombre et l'âge du plus ancien, par invité.
//!
//! La liste coûte un appel par invité ; l'orchestration (`mod.rs`) limite le
//! parallélisme et le nombre d'invités inventoriés par collecte.

use dumbmonit_proto::Sample;

use super::backup::GuestRef;
use super::metrics::gauge;
use super::model::Snapshot;

pub fn snapshot_samples(
    vmid: i64,
    guest: &GuestRef,
    snapshots: &[Snapshot],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let vmid_label = vmid.to_string();
    let mut samples = Vec::new();
    let mut push = |sample: Sample| {
        samples.push(
            sample
                .with_label("vmid", vmid_label.clone())
                .with_label("name", guest.name.clone())
                .with_label("node", guest.node.clone())
                .with_label("type", guest.kind.as_str()),
        );
    };

    // La pseudo-entrée `current` n'a pas de date et n'est pas un instantané :
    // `taken_at` la filtre.
    let dates: Vec<i64> = snapshots.iter().filter_map(Snapshot::taken_at).collect();
    push(gauge("guest_snapshot_count", dates.len() as f64, ts_ms));

    if let (Some(oldest), Some(newest)) = (dates.iter().min(), dates.iter().max()) {
        push(gauge("guest_snapshot_oldest_age_seconds", (now_s - oldest).max(0) as f64, ts_ms));
        push(gauge("guest_snapshot_newest_age_seconds", (now_s - newest).max(0) as f64, ts_ms));
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::proxmox::metrics::GuestKind;
    use crate::collectors::proxmox::model::Envelope;

    /// `GET /nodes/pve1/qemu/100/snapshot` du faux : deux instantanés (3 j et 2 h)
    /// et la pseudo-entrée `current`.
    const QEMU_SNAPSHOTS: &str = r#"{"data":[
      {"name":"snap1","snaptime":1789251437,"description":"before change 1","vmstate":1},
      {"name":"snap2","snaptime":1789503437,"description":"before change 2","vmstate":0,"parent":"snap1"},
      {"name":"current","digest":"8f3a1c9e1ab04000","running":1,"description":"You are here!","parent":"snap2"}
    ]}"#;

    /// Invité sans aucun instantané : seule `current` est renvoyée.
    const NO_SNAPSHOTS: &str = r#"{"data":[
      {"name":"current","digest":"8f3a1c9e1ab04000","running":1,"description":"You are here!"}
    ]}"#;

    const MAINTENANT: i64 = 1789510637;

    fn extraire(json: &str) -> Vec<Snapshot> {
        serde_json::from_str::<Envelope<Vec<Snapshot>>>(json).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    fn routeur() -> GuestRef {
        GuestRef { node: "pve1".into(), name: "router-vm".into(), kind: GuestKind::Qemu }
    }

    #[test]
    fn le_nombre_et_les_ages_extremes_sont_publies_par_invite() {
        let samples =
            snapshot_samples(100, &routeur(), &extraire(QEMU_SNAPSHOTS), MAINTENANT, 1000);
        let labels = r#"{name="router-vm",node="pve1",type="qemu",vmid="100"}"#;

        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_snapshot_count{labels}")),
            Some(2.0),
            "« current » n'est pas un instantané"
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_snapshot_oldest_age_seconds{labels}")),
            Some(259200.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_snapshot_newest_age_seconds{labels}")),
            Some(7200.0)
        );
    }

    #[test]
    fn sans_instantane_seul_le_compte_a_zero_est_publie() {
        let samples = snapshot_samples(100, &routeur(), &extraire(NO_SNAPSHOTS), MAINTENANT, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "proxmox_guest_snapshot_count");
        assert_eq!(samples[0].value, 0.0);
    }
}
