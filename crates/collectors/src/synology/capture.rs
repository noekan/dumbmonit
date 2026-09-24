//! Relecture d'une capture réelle d'API DSM.
//!
//! Les fixtures de `testdata/dsm74/` sont les réponses d'un vrai NAS — un DS918+
//! sous **DSM 7.4.1-90080**, avec Active Backup for Business, Hyper Backup Vault
//! et un cache SSD NVMe — relevées en lecture seule le 24 septembre 2026 et
//! pseudonymisées (numéros de série, UUID, noms de machine remplacés par des
//! étiquettes stables ; la structure est intacte).
//!
//! Ce module existe parce que les jeux d'essai écrits à la main mentaient. Ils
//! reprenaient la forme documentée par les projets communautaires, qui est celle
//! de DSM 6 :
//!
//! * `remain_life` y était un nombre ; DSM 7.4 renvoie
//!   `{"value": 97, "trustable": true}`. Comme [`Num`](super::model::Num) refuse un
//!   objet, **tout `load_info` échouait** : plus un volume, plus un disque, plus une
//!   température sur un vrai NAS, alors que la suite de tests restait verte ;
//! * le drapeau de surchauffe s'appelait `temperature_warn` ; DSM 7.4 le nomme
//!   `temperature_warning` ;
//! * la description d'un volume s'appelait `desc` ; DSM 7.4 la nomme `vol_desc`.
//!
//! D'où la règle que ce module applique : toute structure que le collecteur
//! désérialise est relue ici depuis une réponse réellement observée, et les
//! assertions portent sur des valeurs que l'on peut retrouver dans le fichier.

use super::model::{ApiCatalog, Envelope, RemainLife, StorageInfo, SystemInfo, Utilization};
use super::{abb, backup, devices, metrics};

const API_INFO: &str = include_str!("testdata/dsm74/api-info.json");
const SYSTEM_INFO: &str = include_str!("testdata/dsm74/system-info.json");
const UTILIZATION: &str = include_str!("testdata/dsm74/utilization.json");
const STORAGE: &str = include_str!("testdata/dsm74/storage-load-info.json");

/// Déballe une réponse capturée comme le client le fait en production : si cela
/// échoue, le collecteur échoue.
fn data<T: serde::de::DeserializeOwned>(json: &str) -> T {
    let envelope: Envelope<T> = serde_json::from_str(json).expect("enveloppe illisible");
    assert!(envelope.success, "la capture doit être une réponse en succès");
    envelope.data.expect("réponse sans données")
}

#[test]
fn le_catalogue_reel_donne_a_chaque_api_la_version_que_le_collecteur_demande() {
    let catalog: ApiCatalog = data(API_INFO);

    // Les trois API du cœur, avec la version que le NAS accepte réellement :
    // la capture montre `SYNO.Core.System&version=3&method=info` en succès.
    assert_eq!(catalog.resolve("SYNO.API.Auth", 6).unwrap().version, 6);
    assert_eq!(catalog.resolve("SYNO.Core.System", 3).unwrap().version, 3);
    assert_eq!(catalog.resolve("SYNO.Core.System.Utilization", 1).unwrap().version, 1);
    assert_eq!(catalog.resolve("SYNO.Storage.CGI.Storage", 1).unwrap().version, 1);

    // Active Backup annonce 1–2, et c'est la 1 que le collecteur demande : c'est
    // la seule version pour laquelle les méthodes `list`, `list_result` et
    // `list_device_transfer_size` sont documentées.
    for api in [abb::API_TASK, abb::API_LOG, devices::API_OVERVIEW] {
        let endpoint = catalog.resolve(api, 1).expect("{api} doit être annoncée par ce NAS");
        assert_eq!(endpoint.version, 1, "{api} doit être appelée en version 1");
        assert_eq!(endpoint.path, "entry.cgi");
    }
}

#[test]
fn hyper_backup_absent_du_catalogue_ne_declenche_aucun_appel() {
    // Sur ce NAS, le paquet Hyper Backup est installé mais arrêté : DSM n'annonce
    // alors pas `SYNO.Backup.Task`, et le collecteur doit se taire au lieu
    // d'échouer. C'est le contrat de la vérification de capacité.
    let catalog: ApiCatalog = data(API_INFO);
    assert!(!catalog.contains(super::API_BACKUP), "ce NAS n'annonce pas SYNO.Backup.Task");
    assert!(catalog.resolve(super::API_BACKUP, 1).is_none());
}

#[test]
fn letat_systeme_reel_se_lit_en_entier() {
    let info: SystemInfo = data(SYSTEM_INFO);

    assert_eq!(info.model.as_deref(), Some("DS918+"));
    assert_eq!(info.firmware_ver.as_deref(), Some("DSM 7.4.1-90080"));
    assert_eq!(info.sys_temp.unwrap().0, 40.0);
    assert_eq!(info.ram_size.unwrap().0, 8192.0);
    assert_eq!(info.cpu_cores.unwrap().0, 4.0, "DSM renvoie le nombre de cœurs en chaîne");

    // Le drapeau de surchauffe : DSM 7.4 le nomme `temperature_warning`. Sans
    // l'alias, il était systématiquement absent et la série d'alerte n'existait pas.
    assert_eq!(info.temperature_warn.map(|n| n.0), Some(0.0));
}

#[test]
fn lhorloge_et_la_duree_de_fonctionnement_reelles_sanalysent() {
    let info: SystemInfo = data(SYSTEM_INFO);

    // « 243:52:39 » : des heures sans composante « jours », bien au-delà de 24.
    let uptime = metrics::parse_uptime(info.up_time.as_deref().unwrap()).unwrap();
    assert_eq!(uptime, 243.0 * 3600.0 + 52.0 * 60.0 + 39.0);

    // « 2026-09-24 17:07:45 » : DSM 7 ne renvoie pas la forme `ctime` que la
    // documentation communautaire décrit.
    let clock = backup::parse_nas_clock(info.time.as_deref().unwrap()).unwrap();
    assert_eq!(clock.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-09-24 17:07:45");
}

#[test]
fn lutilisation_reelle_se_lit() {
    let usage: Utilization = data(UTILIZATION);

    let cpu = usage.cpu.expect("bloc cpu");
    assert_eq!(cpu.user_load.unwrap().0, 3.0);
    let memory = usage.memory.expect("bloc mémoire");
    assert_eq!(memory.total_real.unwrap().0, 7_989_256.0, "en kibioctets");
    assert_eq!(memory.real_usage.unwrap().0, 23.0);

    // `total` agrège les interfaces ; les deux liens physiques suivent.
    let interfaces: Vec<&str> = usage.network.iter().filter_map(|n| n.device.as_deref()).collect();
    assert_eq!(interfaces, ["total", "eth0", "eth1"]);
}

#[test]
fn linventaire_du_stockage_reel_se_lit_en_entier() {
    let storage: StorageInfo = data(STORAGE);

    assert_eq!(storage.volumes.len(), 1);
    assert_eq!(storage.disks.len(), 5);
    assert_eq!(storage.storage_pools.len(), 1, "le groupe RAID 5 est dans la même réponse");
    assert_eq!(storage.ssd_caches.len(), 1, "le cache NVMe aussi");

    let volume = &storage.volumes[0];
    assert_eq!(volume.id, "volume_1");
    assert_eq!(volume.vol_path.as_deref(), Some("/volume1"));
    assert_eq!(volume.fs_type.as_deref(), Some("btrfs"));
    assert_eq!(volume.device_type.as_deref(), Some("raid_5"));
    // `vol_desc` et non `desc` : vide ici, mais lue — sans l'alias, une description
    // saisie dans DSM n'apparaîtrait jamais sur la page.
    assert_eq!(volume.desc.as_deref(), Some(""));
    assert_eq!(volume.size.as_ref().unwrap().total.unwrap().0, 57_569_741_635_584.0);

    let pool = &storage.storage_pools[0];
    assert_eq!(pool.id, "reuse_1");
    assert_eq!(pool.status.as_deref(), Some("normal"));
    assert_eq!(pool.device_type.as_deref(), Some("raid_5"));
    assert_eq!(pool.disk_failure_number.unwrap().0, 0.0);

    // Les quatre disques mécaniques ne déclarent pas de durée de vie ; le NVMe si.
    let vies: Vec<Option<f64>> =
        storage.disks.iter().map(|d| d.remain_life.and_then(RemainLife::percent)).collect();
    assert_eq!(vies, [None, None, None, None, Some(97.0)]);

    let nvme = storage.disks.iter().find(|d| d.id == "nvme0n1").expect("le cache NVMe");
    assert_eq!(nvme.smart_status.as_deref(), Some("normal"));
    assert_eq!(nvme.temp.unwrap().0, 36.0);
    assert_eq!(nvme.unc.unwrap().0, -1.0, "un NVMe ne compte pas de secteurs illisibles");

    let sda = storage.disks.iter().find(|d| d.id == "sda").expect("la baie 1");
    assert_eq!(sda.name.as_deref(), Some("Drive 1"));
    assert_eq!(sda.serial.as_deref(), Some("SERIAL-2"), "la capture est pseudonymisée");
    assert_eq!(sda.temp.unwrap().0, 36.0);
    assert_eq!(sda.unc.unwrap().0, 0.0);
    // Ce que DSM 7.4 ne renvoie pas : le drapeau de secteurs défectueux historique.
    // Il est remplacé par `sb_days_left_critical`, que le disque porte bien.
    assert!(sda.exceed_bad_sector_thr.is_none(), "absent de DSM 7.4, d'où l'option");
    assert_eq!(sda.sb_days_left_critical.unwrap().0, 0.0, "le successeur, lui, est là");
    assert_eq!(sda.remain_life_danger.unwrap().0, 0.0);

    let env = storage.env.as_ref().expect("bloc env");
    let status = env.status.as_ref().expect("bloc env.status");
    assert_eq!(status.system_crashed.unwrap().0, 0.0, "DSM 7.4 renvoie un vrai booléen");
    assert_eq!(status.system_need_repair.unwrap().0, 0.0);
    // Les seuils d'occupation ne sont plus dans `env` sur DSM 7.4 : les métriques
    // correspondantes sont simplement absentes, jamais fausses.
    assert!(env.volume_full_warning.is_none());
    assert!(env.volume_full_critical.is_none());
}

#[test]
fn les_metriques_tirees_de_la_capture_reelle_sont_celles_attendues() {
    let storage: StorageInfo = data(STORAGE);
    let ts = 1_700_000_000_000;

    let mut samples = metrics::volume_samples(&storage.volumes, ts);
    samples.extend(metrics::pool_samples(&storage.storage_pools, "pool", ts));
    samples.extend(metrics::pool_samples(&storage.ssd_caches, "ssd_cache", ts));
    samples.extend(metrics::disk_samples(&storage.disks, ts));
    if let Some(env) = &storage.env {
        samples.extend(metrics::env_samples(env, ts));
    }
    samples.push(metrics::storage_health_sample(&storage, ts));

    let valeur = |metric: &str| samples.iter().find(|s| s.metric == metric).map(|s| s.value);

    assert_eq!(valeur("synology_volume_status"), Some(0.0));
    assert_eq!(valeur("synology_pool_status"), Some(0.0));
    assert_eq!(valeur("synology_ssd_cache_status"), Some(0.0));
    assert_eq!(valeur("synology_storage_health"), Some(0.0));
    assert_eq!(valeur("synology_pool_failed_disks"), Some(0.0));

    // Le point qui a motivé tout ce module : l'usure du SSD, publiée pour lui seul.
    let vies: Vec<f64> = samples
        .iter()
        .filter(|s| s.metric == "synology_disk_remaining_life_percent")
        .map(|s| s.value)
        .collect();
    assert_eq!(vies, [97.0], "un seul disque déclare sa durée de vie");

    // Cinq disques, cinq températures : la preuve que la réponse entière a été lue.
    let temperatures =
        samples.iter().filter(|s| s.metric == "synology_disk_temperature_celsius").count();
    assert_eq!(temperatures, 5);

    // `volume_full_warning` n'existe plus sur DSM 7.4 : pas de seuil inventé.
    assert_eq!(valeur("synology_volume_used_warning_percent"), None);

    // Les deux séries de préfaillance existent bien, alimentées par les champs
    // DSM 7.4 : sans elles, deux règles d'alerte livrées ne se déclencheraient
    // plus jamais sur un NAS à jour.
    assert_eq!(
        samples.iter().filter(|s| s.metric == "synology_disk_bad_sector_exceeded").count(),
        5
    );
    assert_eq!(
        samples.iter().filter(|s| s.metric == "synology_disk_life_below_threshold").count(),
        5
    );
}
