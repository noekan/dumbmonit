//! Conversion des réponses de l'API en échantillons.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//! C'est ici que se décide le choix `Gauge` / `Counter`, et lui seul détermine si
//! un graphe affichera une valeur ou un débit.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    CpuUsage, Disk, MemoryUsage, Num, StorageEnv, StorageInfo, SystemInfo, Utilization, Volume,
};

/// Préfixe commun à toutes les métriques de l'intégration.
///
/// Il n'est pas redondant avec le préfixe `dumbmonit_` ajouté à l'écriture : ce
/// dernier isole l'outil, celui-ci isole l'intégration. Sans lui, `disk_temperature`
/// entrerait en collision avec la même notion venue de SNMP — et un NAS Synology est
/// justement l'équipement qu'on surveille volontiers par les deux voies à la fois.
const P: &str = "synology_";

/// Un kibioctet. DSM exprime en kibioctets tout ce que renvoie l'API
/// d'utilisation ; on convertit en octets pour rester homogène avec le reste
/// d'DumbMonit, où toutes les tailles sont en octets.
const KIB: f64 = 1024.0;

/// Un mébioctet, unité de `ram_size`.
const MIB: f64 = 1024.0 * 1024.0;

fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Pourcentage d'occupation, ou `None` si le total est inconnu ou nul — mieux vaut
/// pas de point du tout qu'un 0 % trompeur sur un volume en cours de création.
fn percent(used: Option<Num>, total: Option<Num>) -> Option<f64> {
    let total = total?.0;
    let used = used?.0;
    (total > 0.0).then(|| used / total * 100.0)
}

fn flag_value(value: Option<Num>) -> f64 {
    if Num::flag(value) { 1.0 } else { 0.0 }
}

/// Nettoie une chaîne d'identification renvoyée par DSM.
///
/// Les champs qui viennent du micrologiciel des disques sont complétés à droite
/// par des espaces — `"ST4000VN008-2DR166      "`, `"WDC     "` — parce qu'ils sont
/// lus tels quels dans la table d'identification ATA. Non nettoyés, ils feraient
/// deux séries distinctes pour un même modèle selon la version de DSM.
fn clean(value: &Option<String>) -> String {
    value.as_deref().unwrap_or_default().trim().to_string()
}

/// Gravité d'un état textuel de DSM, sur une échelle commune aux volumes et aux
/// disques : `0` normal, `1` à surveiller, `2` critique.
///
/// L'échelle existe parce qu'une chaîne de caractères ne se compare pas dans une
/// règle d'alerte. Le libellé d'origine reste porté en étiquette : c'est lui que
/// l'utilisateur lit, la valeur ne sert qu'à déclencher.
///
/// Seule la valeur `normal` est attestée par des relevés réels — un NAS en bonne
/// santé ne dit rien d'autre. Les libellés de panne proviennent du vocabulaire de
/// DSM et non d'une capture, et le vocabulaire a déjà changé d'une version majeure
/// à l'autre. D'où le repli : **tout état non reconnu vaut `1`**, jamais `0`.
/// Classer l'inconnu comme normal masquerait un état introduit par une version
/// plus récente ; le classer comme critique réveillerait pour rien.
pub fn severity(status: &str) -> f64 {
    match status.trim().to_ascii_lowercase().as_str() {
        "normal" | "healthy" | "health" | "ok" | "good" | "safe" | "online" => 0.0,
        // États transitoires ou avertissements : le stockage fonctionne, mais une
        // opération est en cours ou un seuil est approché.
        "background" | "attention" | "warning" | "checking" | "scrubbing" | "syncing"
        | "raid_syncing" | "migrating" | "expanding" | "converting" | "creating" | "deleting"
        | "init" | "initialized" | "initializing" | "detect" | "testing" | "verifying"
        | "unknown" => 1.0,
        // Perte de redondance, panne matérielle ou volume inutilisable.
        "crashed"
        | "critical"
        | "failing"
        | "fail"
        | "failed"
        | "error"
        | "damaged"
        | "degrade"
        | "degraded"
        | "broken"
        | "system_partition_failed"
        | "unformatted"
        | "not_initialized"
        | "offline" => 2.0,
        _ => 1.0,
    }
}

/// Convertit la durée de fonctionnement renvoyée par DSM en secondes.
///
/// DSM la donne en texte, sous la forme `"75:12:9"` : **heures totales**, minutes,
/// secondes, sans remplissage par des zéros et sans composante « jours » — le
/// champ des heures dépasse donc allègrement 24. Analyser cela comme une date
/// échouerait ; on découpe donc sur les deux-points, en lisant les facteurs depuis
/// la fin. La forme à quatre champs, préfixée des jours, et la forme purement
/// numérique en secondes sont acceptées par la même mécanique, au cas où une
/// version ou un modèle s'en écarte.
pub fn parse_uptime(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Forme numérique : déjà des secondes.
    if !raw.contains(':') {
        return raw.parse::<f64>().ok().filter(|value| *value >= 0.0);
    }

    let parts: Vec<f64> =
        raw.split(':').map(|part| part.trim().parse::<f64>().ok()).collect::<Option<_>>()?;
    // Les facteurs sont lus depuis la fin : secondes, minutes, heures, jours.
    let facteurs = [1.0, 60.0, 3600.0, 86_400.0];
    if parts.is_empty() || parts.len() > facteurs.len() {
        return None;
    }
    let total =
        parts.iter().rev().zip(facteurs).map(|(value, facteur)| value * facteur).sum::<f64>();
    (total >= 0.0).then_some(total)
}

/// `SYNO.Core.System&method=info`.
///
/// Le modèle, la version de DSM et le numéro de série ne sont pas des mesures :
/// ils partent dans une série de présence `*_info` valant toujours 1, selon la
/// convention où seules les étiquettes portent l'information. Les coller sur les
/// métriques chiffrées obligerait à réécrire toute la série à la moindre mise à
/// jour de DSM.
pub fn system_samples(info: &SystemInfo, ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        gauge("system_info", 1.0, ts_ms)
            .with_label("model", clean(&info.model))
            .with_label("dsm_version", clean(&info.firmware_ver))
            .with_label("serial", clean(&info.serial))
            .with_label("cpu", clean(&info.cpu_family)),
    ];

    // La durée de fonctionnement est un `Gauge` et non un `Counter` : elle repart
    // de zéro à chaque redémarrage, et c'est justement cette chute que l'on veut
    // voir telle quelle plutôt que lissée en débit.
    if let Some(uptime) = info.up_time.as_deref().and_then(parse_uptime) {
        samples.push(gauge("uptime_seconds", uptime, ts_ms));
    }
    if let Some(temp) = info.sys_temp {
        samples.push(gauge("temperature_celsius", temp.0, ts_ms));
    }
    // Publié même à zéro : c'est une série d'alerte, elle doit exister en
    // permanence pour que son passage à 1 soit détectable.
    if info.temperature_warn.is_some() {
        samples.push(gauge("temperature_warning", flag_value(info.temperature_warn), ts_ms));
    }
    if let Some(cores) = info.cpu_cores {
        samples.push(gauge("cpu_cores", cores.0, ts_ms));
    }
    if let Some(speed) = info.cpu_clock_speed {
        samples.push(gauge("cpu_clock_mhz", speed.0, ts_ms));
    }
    if let Some(ram) = info.ram_size {
        samples.push(gauge("memory_installed_bytes", ram.0 * MIB, ts_ms));
    }

    samples
}

/// `SYNO.Core.System.Utilization&method=get`.
pub fn utilization_samples(usage: &Utilization, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    if let Some(cpu) = &usage.cpu {
        samples.extend(cpu_samples(cpu, ts_ms));
    }
    if let Some(memory) = &usage.memory {
        samples.extend(memory_samples(memory, ts_ms));
    }

    for interface in &usage.network {
        let Some(device) = interface.device.clone() else { continue };
        // `rx` et `tx` sont des débits instantanés calculés par DSM, pas des
        // compteurs cumulés : les déclarer en `Counter` ferait dériver le taux
        // affiché par le lecteur, qui recalculerait un débit sur un débit.
        for (metric, value) in [
            ("network_rx_bytes_per_second", interface.rx),
            ("network_tx_bytes_per_second", interface.tx),
        ] {
            if let Some(value) = value {
                samples.push(gauge(metric, value.0, ts_ms).with_label("interface", device.clone()));
            }
        }
    }

    samples
}

fn cpu_samples(cpu: &CpuUsage, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for (metric, value) in [
        ("cpu_user_percent", cpu.user_load),
        ("cpu_system_percent", cpu.system_load),
        ("cpu_other_percent", cpu.other_load),
    ] {
        if let Some(value) = value {
            samples.push(gauge(metric, value.0, ts_ms));
        }
    }

    // DSM n'expose pas d'occupation totale : c'est la somme des trois charges,
    // exactement ce qu'affiche le moniteur de ressources. On la calcule ici plutôt
    // que de laisser chaque graphe et chaque règle d'alerte refaire l'addition.
    if cpu.user_load.is_some() || cpu.system_load.is_some() || cpu.other_load.is_some() {
        let total = Num::get(cpu.user_load, 0.0)
            + Num::get(cpu.system_load, 0.0)
            + Num::get(cpu.other_load, 0.0);
        samples.push(gauge("cpu_usage_percent", total.min(100.0), ts_ms));
    }

    // `1min_load`, `5min_load` et `15min_load` sont volontairement laissées de
    // côté. Synology ne documente pas leur échelle, et les relevés la contredisent :
    // un NAS à 9 % d'occupation annonce 37, 33 et 51, ce qui n'est ni un pourcentage
    // ni une charge moyenne au sens habituel. Publier un nombre dont l'unité est
    // inconnue produirait des graphes et des seuils faux ; l'occupation ci-dessus
    // couvre déjà le besoin.

    samples
}

fn memory_samples(memory: &MemoryUsage, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for (metric, value) in [
        ("memory_total_bytes", memory.total_real),
        ("memory_available_bytes", memory.avail_real),
        ("memory_cached_bytes", memory.cached),
        ("memory_buffer_bytes", memory.buffer),
        ("swap_total_bytes", memory.total_swap),
        ("swap_available_bytes", memory.avail_swap),
    ] {
        if let Some(value) = value {
            samples.push(gauge(metric, value.0 * KIB, ts_ms));
        }
    }

    // DSM ne donne que le disponible : l'utilisé se déduit, et c'est lui qu'on
    // trace. Le calcul est fait ici pour qu'il ne soit pas refait, différemment,
    // dans chaque graphe.
    if let (Some(total), Some(avail)) = (memory.total_real, memory.avail_real) {
        samples.push(gauge("memory_used_bytes", (total.0 - avail.0).max(0.0) * KIB, ts_ms));
    }
    if let (Some(total), Some(avail)) = (memory.total_swap, memory.avail_swap) {
        samples.push(gauge("swap_used_bytes", (total.0 - avail.0).max(0.0) * KIB, ts_ms));
    }

    // `real_usage` est le pourcentage tel que DSM l'affiche, mémoire cache exclue :
    // on le reprend plutôt que de recalculer, pour que la valeur d'DumbMonit
    // corresponde à celle lue dans l'interface du NAS.
    if let Some(usage) = memory.real_usage {
        samples.push(gauge("memory_usage_percent", usage.0, ts_ms));
    }
    if let Some(usage) = memory.swap_usage {
        samples.push(gauge("swap_usage_percent", usage.0, ts_ms));
    }

    samples
}

/// Volumes de `SYNO.Storage.CGI.Storage&method=load_info`.
pub fn volume_samples(volumes: &[Volume], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for volume in volumes {
        // DSM n'a pas de nom d'affichage garanti : `desc` est une description libre
        // le plus souvent vide, et c'est le point de montage que l'utilisateur
        // reconnaît. On retombe sur l'identifiant en dernier recours.
        let name = [clean(&volume.desc), clean(&volume.vol_path)]
            .into_iter()
            .find(|candidat| !candidat.is_empty())
            .unwrap_or_else(|| volume.id.clone());
        let fs_type = clean(&volume.fs_type);
        let raid_type = clean(&volume.device_type);
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("volume", volume.id.clone())
                    .with_label("name", name.clone())
                    .with_label("fs_type", fs_type.clone())
                    .with_label("raid_type", raid_type.clone()),
            );
        };

        // L'état est publié même quand les tailles manquent : un volume écroulé
        // n'annonce plus sa capacité, et c'est précisément là qu'il faut alerter.
        let status = volume.status.clone().unwrap_or_else(|| "unknown".to_string());
        push(gauge("volume_status", severity(&status), ts_ms).with_label("status", status));

        let Some(size) = &volume.size else { continue };
        if let Some(total) = size.total {
            push(gauge("volume_total_bytes", total.0, ts_ms));
        }
        if let Some(used) = size.used {
            push(gauge("volume_used_bytes", used.0, ts_ms));
        }
        if let (Some(total), Some(used)) = (size.total, size.used) {
            push(gauge("volume_available_bytes", (total.0 - used.0).max(0.0), ts_ms));
        }
        if let Some(ratio) = percent(size.used, size.total) {
            push(gauge("volume_used_percent", ratio, ts_ms));
        }
    }

    samples
}

/// Disques de `SYNO.Storage.CGI.Storage&method=load_info`.
///
/// C'est la partie la plus utile de l'intégration : un disque qui chauffe ou dont
/// le S.M.A.R.T. se dégrade annonce une panne des semaines à l'avance, bien avant
/// que le volume ne bouge.
pub fn disk_samples(disks: &[Disk], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for disk in disks {
        let name = match clean(&disk.name) {
            libelle if libelle.is_empty() => disk.id.clone(),
            libelle => libelle,
        };
        let mut push = |sample: Sample| {
            samples
                .push(sample.with_label("disk", disk.id.clone()).with_label("name", name.clone()));
        };

        // Modèle, numéro de série et micrologiciel vont dans une série de présence
        // à part : sur les séries chiffrées, ils changeraient d'identité au premier
        // remplacement de disque et couperaient l'historique de la baie.
        push(
            gauge("disk_info", 1.0, ts_ms)
                .with_label("model", clean(&disk.model))
                .with_label("serial", clean(&disk.serial))
                .with_label("vendor", clean(&disk.vendor))
                .with_label("firmware", clean(&disk.firm))
                .with_label("type", clean(&disk.disk_type))
                .with_label("ssd", if Num::flag(disk.is_ssd) { "1" } else { "0" }),
        );

        let status = disk.status.clone().unwrap_or_else(|| "unknown".to_string());
        push(gauge("disk_status", severity(&status), ts_ms).with_label("status", status));

        let smart = disk.smart_status.clone().unwrap_or_else(|| "unknown".to_string());
        push(gauge("disk_smart_status", severity(&smart), ts_ms).with_label("smart_status", smart));

        if let Some(temp) = disk.temp {
            push(gauge("disk_temperature_celsius", temp.0, ts_ms));
        }
        if let Some(size) = disk.size_total {
            push(gauge("disk_size_bytes", size.0, ts_ms));
        }
        // Ces deux drapeaux sont les signaux de préfaillance les plus directs que
        // DSM expose : ils sont publiés même à zéro, pour que leur passage à 1 soit
        // visible dans une règle d'alerte.
        if disk.exceed_bad_sector_thr.is_some() {
            push(gauge("disk_bad_sector_exceeded", flag_value(disk.exceed_bad_sector_thr), ts_ms));
        }
        if disk.below_remain_life_thr.is_some() {
            push(gauge("disk_life_below_threshold", flag_value(disk.below_remain_life_thr), ts_ms));
        }
        // L'usure d'un SSD : DSM donne `-1` pour un disque mécanique, qui n'a
        // pas de durée de vie déclarée — pas de point plutôt qu'un -1 % absurde.
        if let Some(life) = disk.remain_life.filter(|n| (0.0..=100.0).contains(&n.0)) {
            push(gauge("disk_remaining_life_percent", life.0, ts_ms));
        }
        // Publié même à zéro : c'est sa croissance qu'une règle surveille.
        if let Some(unc) = disk.unc.filter(|n| n.0 >= 0.0) {
            push(gauge("disk_unc_count", unc.0, ts_ms));
        }
    }

    samples
}

/// Bloc `env` de `load_info` : verdict du NAS sur lui-même et seuils qu'il applique.
///
/// Ces seuils valent mieux qu'une valeur codée en dur dans DumbMonit : ce sont ceux
/// que l'utilisateur a réglés dans DSM, donc ceux qui correspondent à ce que le NAS
/// lui affiche déjà. Les publier permet à une règle d'alerte de s'y référer plutôt
/// que d'imposer un 80 % arbitraire.
pub fn env_samples(env: &StorageEnv, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    if let Some(status) = &env.status {
        samples.push(gauge("system_crashed", flag_value(status.system_crashed), ts_ms));
        samples.push(gauge("system_need_repair", flag_value(status.system_need_repair), ts_ms));
    }

    // DSM exprime ces seuils en fraction d'espace **libre** : 0,2 veut dire
    // « prévenir quand il reste 20 % », soit 80 % d'occupation. On les convertit en
    // pourcentage d'occupation, la grandeur que l'on trace effectivement.
    for (metric, fraction) in [
        ("volume_used_warning_percent", env.volume_full_warning),
        ("volume_used_critical_percent", env.volume_full_critical),
    ] {
        if let Some(fraction) = fraction.filter(|f| (0.0..=1.0).contains(&f.0)) {
            samples.push(gauge(metric, (1.0 - fraction.0) * 100.0, ts_ms));
        }
    }

    samples
}

/// Santé globale du stockage, calculée à partir des états déjà collectés.
///
/// DSM affiche un état de santé d'ensemble dans son centre d'informations, et
/// `SYNO.Core.System.SystemHealth` existe bien — mais elle ne renvoie ni
/// température, ni état de disque, ni ventilateur, seulement un verdict opaque.
/// Plutôt que d'inventer des noms de champs, on reconstitue la même idée à partir
/// des états de volumes et de disques, qui sont eux parfaitement définis. La valeur
/// suit l'échelle de [`severity`], et vaut donc la pire situation constatée.
pub fn storage_health_sample(storage: &StorageInfo, ts_ms: i64) -> Sample {
    let systeme = storage.env.as_ref().and_then(|env| env.status.as_ref()).map_or(0.0, |status| {
        if Num::flag(status.system_crashed) {
            2.0
        } else if Num::flag(status.system_need_repair) {
            1.0
        } else {
            0.0
        }
    });

    let pire = storage
        .volumes
        .iter()
        .map(|volume| severity(volume.status.as_deref().unwrap_or("unknown")))
        .chain(storage.disks.iter().flat_map(|disk| {
            [
                severity(disk.status.as_deref().unwrap_or("unknown")),
                severity(disk.smart_status.as_deref().unwrap_or("unknown")),
            ]
        }))
        .fold(systeme, f64::max);

    gauge("storage_health", pire, ts_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synology::model::Envelope;

    /// `SYNO.Core.System&method=info` sur un DS920+ sous DSM 7.
    ///
    /// Noter `up_time` : soixante-quinze heures, sans composante « jours » et sans
    /// zéro de remplissage sur les secondes. C'est la forme que renvoie réellement
    /// DSM, et elle a de quoi piéger un analyseur de date.
    const SYSTEM_INFO: &str = r#"{"data":{
      "cpu_clock_speed":2000,
      "cpu_cores":"4",
      "cpu_family":"Celeron J4125",
      "cpu_series":"J4125",
      "enabled_ntp":true,
      "firmware_ver":"DSM 7.2.1-69057 Update 5",
      "model":"DS920+",
      "ntp_server":"pool.ntp.org",
      "ram_size":20480,
      "serial":"2040PDN123456",
      "sys_temp":41,
      "temperature_warn":false,
      "time":"Sun Aug 31 14:22:03 2025",
      "time_zone":"Amsterdam",
      "up_time":"75:12:9",
      "usb_dev":[]
    },"success":true}"#;

    /// `SYNO.Core.System.Utilization&method=get`.
    ///
    /// Les tailles mémoire sont en kibioctets, et `rx` / `tx` sont des débits
    /// instantanés : sur un NAS allumé depuis soixante-quinze heures, un compteur
    /// cumulé se compterait en milliards.
    const UTILIZATION: &str = r#"{"data":{
      "cpu":{"15min_load":51,"1min_load":37,"5min_load":33,"device":"System",
             "other_load":2,"system_load":5,"user_load":11},
      "memory":{"avail_real":4194304,"avail_swap":2097152,"buffer":262144,"cached":8388608,
                "device":"Memory","memory_size":20971520,"real_usage":48,"si_disk":0,"so_disk":0,
                "swap_usage":13,"total_real":20447232,"total_swap":2410724},
      "network":[{"device":"total","rx":152340,"tx":98211},
                 {"device":"eth0","rx":152340,"tx":98211},
                 {"device":"eth1","rx":0,"tx":0}],
      "time":1725110523
    },"success":true}"#;

    /// `SYNO.Storage.CGI.Storage&method=load_info` : deux volumes, quatre disques,
    /// dont un en préfaillance S.M.A.R.T. et un écroulé.
    ///
    /// `model` et `vendor` sont complétés à droite par des espaces, exactement comme
    /// le fait DSM en recopiant la table d'identification ATA. Les tailles sont des
    /// chaînes d'octets, et les volumes n'ont pas de nom d'affichage : seuls `desc`
    /// et `vol_path` existent.
    const STORAGE: &str = r#"{"data":{
      "disks":[
        {"below_remain_life_thr":false,"container":{"order":0,"str":"Interne","type":"internal"},
         "device":"/dev/sata1","diskType":"SATA","exceed_bad_sector_thr":false,
         "firm":"SC61","id":"sata1","model":"ST8000VN004-2M2101      ","name":"Disque 1",
         "serial":"WKD0AB12","size_total":"8001563222016","smart_status":"normal",
         "status":"normal","temp":38,"vendor":"Seagate ","remain_life":-1,"unc":0,"isSsd":false},
        {"below_remain_life_thr":false,"container":{"order":1,"str":"Interne","type":"internal"},
         "device":"/dev/sata2","diskType":"SATA","exceed_bad_sector_thr":true,
         "firm":"SC61","id":"sata2","model":"ST8000VN004-2M2101      ","name":"Disque 2",
         "serial":"WKD0CD34","size_total":"8001563222016","smart_status":"warning",
         "status":"normal","temp":46,"vendor":"Seagate "},
        {"below_remain_life_thr":false,"container":{"order":2,"str":"Interne","type":"internal"},
         "device":"/dev/sata3","diskType":"SATA","exceed_bad_sector_thr":false,
         "firm":"SC61","id":"sata3","model":"ST8000VN004-2M2101      ","name":"Disque 3",
         "serial":"WKD0EF56","size_total":"8001563222016","smart_status":"normal",
         "status":"crashed","temp":39,"vendor":"Seagate "},
        {"below_remain_life_thr":true,"container":{"order":0,"str":"Interne","type":"internal"},
         "device":"/dev/nvme0n1","diskType":"SSD","exceed_bad_sector_thr":false,
         "firm":"EXA7301Q","id":"nvme0n1","model":"Samsung SSD 970 EVO Plus 500GB",
         "name":"Cache SSD 1","serial":"S4EVNF0N123456","size_total":"500107862016",
         "smart_status":"normal","status":"normal","temp":52,"vendor":"Samsung ",
         "remain_life":7,"unc":3,"isSsd":true}
      ],
      "env":{"bay_number":"4","max_volume_count":64,
             "volume_full_critical":0.1,"volume_full_warning":0.2,
             "status":{"system_crashed":false,"system_need_repair":false}},
      "storagePools":[],
      "volumes":[
        {"device_type":"shr_1","desc":"","fs_type":"btrfs","id":"volume_1","vol_path":"/volume1",
         "size":{"free_inode":"975175424","total":"14371964157952","total_device":"2",
                 "total_inode":"976562500","used":"11497571326464"},
         "status":"normal"},
        {"device_type":"raid_1","desc":"Archives froides","fs_type":"ext4","id":"volume_2",
         "vol_path":"/volume2",
         "size":{"total":"7943432896512","used":"402653184000"},
         "status":"degrade"}
      ]
    },"success":true}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data.unwrap()
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    fn premiere(samples: &[Sample], metric: &str) -> Option<f64> {
        samples.iter().find(|s| s.metric == metric).map(|s| s.value)
    }

    #[test]
    fn la_duree_de_fonctionnement_suit_la_forme_reelle_de_dsm() {
        // Heures totales, minutes, secondes, sans zéro de remplissage.
        assert_eq!(parse_uptime("75:12:9"), Some(75.0 * 3600.0 + 12.0 * 60.0 + 9.0));
        // Le champ des heures dépasse allègrement vingt-quatre.
        assert_eq!(parse_uptime("1826:04:00"), Some(1826.0 * 3600.0 + 240.0));
        // Formes de repli, au cas où un modèle ou une version s'en écarte.
        assert_eq!(parse_uptime("113:16:07:32"), Some(9_821_252.0));
        assert_eq!(parse_uptime("58052"), Some(58_052.0));
        assert_eq!(parse_uptime("  75:12:9  "), Some(270_729.0));
    }

    #[test]
    fn une_duree_de_fonctionnement_illisible_ne_produit_pas_de_metrique_fausse() {
        for illisible in ["", "   ", "inconnu", "1:2:3:4:5", "a:b:c", "-5"] {
            assert_eq!(parse_uptime(illisible), None, "« {illisible} » aurait dû être refusé");
        }
    }

    #[test]
    fn letat_systeme_donne_identite_temperature_et_disponibilite() {
        let info: SystemInfo = extraire(SYSTEM_INFO);
        let samples = system_samples(&info, 1000);

        let identite =
            samples.iter().find(|s| s.metric == "synology_system_info").expect("série de présence");
        assert_eq!(identite.value, 1.0);
        assert_eq!(identite.labels["model"], "DS920+");
        assert_eq!(identite.labels["dsm_version"], "DSM 7.2.1-69057 Update 5");
        assert_eq!(identite.labels["serial"], "2040PDN123456");

        assert_eq!(premiere(&samples, "synology_temperature_celsius"), Some(41.0));
        assert_eq!(premiere(&samples, "synology_temperature_warning"), Some(0.0));
        assert_eq!(premiere(&samples, "synology_uptime_seconds"), Some(270_729.0));
        assert_eq!(
            premiere(&samples, "synology_cpu_cores"),
            Some(4.0),
            "cpu_cores arrive en chaîne"
        );
        assert_eq!(
            premiere(&samples, "synology_memory_installed_bytes"),
            Some(20480.0 * MIB),
            "ram_size est en mébioctets, contrairement au bloc d'utilisation"
        );
    }

    #[test]
    fn la_disponibilite_est_une_jauge_et_non_un_compteur() {
        // Un `Counter` serait lu comme un débit : le redémarrage du NAS, qui remet
        // la durée à zéro, deviendrait invisible au lieu d'être flagrant.
        let info: SystemInfo = extraire(SYSTEM_INFO);
        let samples = system_samples(&info, 1000);
        let uptime = samples.iter().find(|s| s.metric == "synology_uptime_seconds").unwrap();
        assert_eq!(uptime.kind, MetricKind::Gauge);
    }

    #[test]
    fn un_nas_sans_sonde_de_temperature_ne_produit_pas_de_zero_trompeur() {
        let info: SystemInfo = extraire(r#"{"data":{"model":"DS220j"},"success":true}"#);
        let samples = system_samples(&info, 1000);

        assert!(samples.iter().all(|s| s.metric != "synology_temperature_celsius"));
        assert!(samples.iter().all(|s| s.metric != "synology_uptime_seconds"));
        assert_eq!(samples.len(), 1, "seule la série de présence subsiste");
    }

    #[test]
    fn la_charge_processeur_est_la_somme_des_trois_composantes() {
        let usage: Utilization = extraire(UTILIZATION);
        let samples = utilization_samples(&usage, 1000);

        assert_eq!(premiere(&samples, "synology_cpu_user_percent"), Some(11.0));
        assert_eq!(premiere(&samples, "synology_cpu_system_percent"), Some(5.0));
        assert_eq!(premiere(&samples, "synology_cpu_other_percent"), Some(2.0));
        assert_eq!(premiere(&samples, "synology_cpu_usage_percent"), Some(18.0));
    }

    #[test]
    fn les_charges_moyennes_de_dsm_ne_sont_pas_publiees() {
        // Le NAS de la capture est à 18 % d'occupation et annonce pourtant 37, 33 et
        // 51 : l'échelle de ces trois champs n'est ni documentée ni déductible.
        // Publier un nombre sans unité produirait des seuils d'alerte faux.
        let usage: Utilization = extraire(UTILIZATION);
        let samples = utilization_samples(&usage, 1000);
        assert!(
            samples.iter().all(|s| !s.metric.contains("load")),
            "aucune charge moyenne ne doit être publiée"
        );
    }

    #[test]
    fn la_memoire_est_convertie_en_octets_et_lutilise_est_deduit() {
        let usage: Utilization = extraire(UTILIZATION);
        let samples = utilization_samples(&usage, 1000);

        assert_eq!(premiere(&samples, "synology_memory_total_bytes"), Some(20_447_232.0 * KIB));
        assert_eq!(premiere(&samples, "synology_memory_available_bytes"), Some(4_194_304.0 * KIB));
        assert_eq!(
            premiere(&samples, "synology_memory_used_bytes"),
            Some((20_447_232.0 - 4_194_304.0) * KIB)
        );
        assert_eq!(premiere(&samples, "synology_memory_usage_percent"), Some(48.0));
        assert_eq!(premiere(&samples, "synology_swap_usage_percent"), Some(13.0));
    }

    #[test]
    fn le_trafic_reseau_est_un_debit_instantane_donc_une_jauge() {
        let usage: Utilization = extraire(UTILIZATION);
        let samples = utilization_samples(&usage, 1000);

        assert_eq!(
            valeur(&samples, r#"synology_network_rx_bytes_per_second{interface="eth0"}"#),
            Some(152_340.0)
        );
        assert_eq!(
            valeur(&samples, r#"synology_network_tx_bytes_per_second{interface="total"}"#),
            Some(98_211.0)
        );
        assert!(
            samples
                .iter()
                .filter(|s| s.metric.starts_with("synology_network_"))
                .all(|s| s.kind == MetricKind::Gauge)
        );
    }

    #[test]
    fn une_reponse_dutilisation_amputee_ne_fait_pas_echouer_la_conversion() {
        let usage: Utilization = extraire(r#"{"data":{"cpu":{"user_load":7}},"success":true}"#);
        let samples = utilization_samples(&usage, 1000);
        assert_eq!(premiere(&samples, "synology_cpu_usage_percent"), Some(7.0));
        assert!(samples.iter().all(|s| !s.metric.starts_with("synology_memory_")));
    }

    #[test]
    fn un_volume_sans_description_est_nomme_par_son_point_de_montage() {
        // DSM n'a pas de champ de nom d'affichage : `desc` est libre et souvent vide.
        let storage: StorageInfo = extraire(STORAGE);
        let samples = volume_samples(&storage.volumes, 1000);

        let premier = samples.iter().find(|s| s.labels["volume"] == "volume_1").unwrap();
        assert_eq!(premier.labels["name"], "/volume1");
        let second = samples.iter().find(|s| s.labels["volume"] == "volume_2").unwrap();
        assert_eq!(second.labels["name"], "Archives froides");
    }

    #[test]
    fn les_volumes_portent_leur_capacite_et_leur_type_de_raid() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = volume_samples(&storage.volumes, 1000);

        let cle = r#"synology_volume_used_percent{fs_type="btrfs",name="/volume1",raid_type="shr_1",volume="volume_1"}"#;
        let occupation = valeur(&samples, cle).expect("l'occupation du volume 1");
        assert!((occupation - 80.0).abs() < 0.1, "occupation à {occupation} %");

        assert_eq!(
            valeur(
                &samples,
                r#"synology_volume_total_bytes{fs_type="btrfs",name="/volume1",raid_type="shr_1",volume="volume_1"}"#
            ),
            Some(14_371_964_157_952.0),
            "les tailles arrivent en chaînes et doivent être converties"
        );
    }

    #[test]
    fn un_volume_degrade_est_signale_comme_critique_avec_son_libelle() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = volume_samples(&storage.volumes, 1000);

        let etat = samples
            .iter()
            .find(|s| s.metric == "synology_volume_status" && s.labels["volume"] == "volume_2")
            .expect("l'état du volume 2");
        assert_eq!(etat.value, 2.0, "un volume dégradé a perdu sa redondance");
        assert_eq!(etat.labels["status"], "degrade", "le libellé d'origine reste lisible");

        let sain = samples
            .iter()
            .find(|s| s.metric == "synology_volume_status" && s.labels["volume"] == "volume_1")
            .unwrap();
        assert_eq!(sain.value, 0.0);
    }

    #[test]
    fn letat_dun_volume_est_publie_meme_sans_capacite() {
        // Un volume écroulé n'annonce plus sa taille : c'est justement le moment où
        // son état doit remonter.
        let storage: StorageInfo = extraire(
            r#"{"data":{"volumes":[{"id":"volume_3","status":"crashed"}],"disks":[]},"success":true}"#,
        );
        let samples = volume_samples(&storage.volumes, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value, 2.0);
        assert_eq!(samples[0].labels["name"], "volume_3", "repli sur l'identifiant");
    }

    #[test]
    fn un_disque_en_prefaillance_smart_est_reperable_par_une_seule_regle() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);

        let smart: Vec<(&str, f64)> = samples
            .iter()
            .filter(|s| s.metric == "synology_disk_smart_status")
            .map(|s| (s.labels["disk"].as_str(), s.value))
            .collect();
        assert!(smart.contains(&("sata2", 1.0)), "le disque en avertissement : {smart:?}");
        assert!(smart.contains(&("sata1", 0.0)), "les disques sains restent à zéro");

        // La règle « disque en préfaillance » se ramène à un seul seuil.
        let suspects: Vec<&str> =
            smart.iter().filter(|(_, valeur)| *valeur >= 1.0).map(|(disque, _)| *disque).collect();
        assert_eq!(suspects, vec!["sata2"]);
    }

    #[test]
    fn les_seuils_de_secteurs_et_de_duree_de_vie_sont_publies_meme_a_zero() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);

        let secteurs: Vec<(&str, f64)> = samples
            .iter()
            .filter(|s| s.metric == "synology_disk_bad_sector_exceeded")
            .map(|s| (s.labels["disk"].as_str(), s.value))
            .collect();
        assert_eq!(secteurs.len(), 4, "une série par disque, y compris à zéro");
        assert!(secteurs.contains(&("sata2", 1.0)));
        assert!(secteurs.contains(&("sata1", 0.0)));

        let vie = samples
            .iter()
            .find(|s| {
                s.metric == "synology_disk_life_below_threshold" && s.labels["disk"] == "nvme0n1"
            })
            .unwrap();
        assert_eq!(vie.value, 1.0, "le SSD arrive en fin de vie");
    }

    #[test]
    fn lusure_dun_ssd_et_les_secteurs_illisibles_sont_publies_tels_quels() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);

        let vie: Vec<(&str, f64)> = samples
            .iter()
            .filter(|s| s.metric == "synology_disk_remaining_life_percent")
            .map(|s| (s.labels["disk"].as_str(), s.value))
            .collect();
        assert_eq!(vie, vec![("nvme0n1", 7.0)], "un -1 de disque mécanique n'est pas une usure");

        let unc: Vec<(&str, f64)> = samples
            .iter()
            .filter(|s| s.metric == "synology_disk_unc_count")
            .map(|s| (s.labels["disk"].as_str(), s.value))
            .collect();
        assert!(unc.contains(&("sata1", 0.0)), "publié même à zéro : {unc:?}");
        assert!(unc.contains(&("nvme0n1", 3.0)));
        assert_eq!(unc.len(), 2, "les disques sans compteur n'inventent rien");

        let info = |disk: &str| {
            samples
                .iter()
                .find(|s| s.metric == "synology_disk_info" && s.labels["disk"] == disk)
                .unwrap()
                .labels["ssd"]
                .clone()
        };
        assert_eq!(info("nvme0n1"), "1");
        assert_eq!(info("sata1"), "0");
    }

    #[test]
    fn un_disque_ecroule_est_critique() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);

        let etat = samples
            .iter()
            .find(|s| s.metric == "synology_disk_status" && s.labels["disk"] == "sata3")
            .unwrap();
        assert_eq!(etat.value, 2.0);
        assert_eq!(etat.labels["status"], "crashed");
    }

    #[test]
    fn le_modele_et_le_numero_de_serie_vivent_dans_une_serie_a_part_et_sont_elages() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);

        let identite = samples
            .iter()
            .find(|s| s.metric == "synology_disk_info" && s.labels["disk"] == "sata1")
            .unwrap();
        assert_eq!(identite.value, 1.0);
        assert_eq!(
            identite.labels["model"], "ST8000VN004-2M2101",
            "DSM complète le modèle par des espaces, elles ne doivent pas entrer dans la série"
        );
        assert_eq!(identite.labels["vendor"], "Seagate");
        assert_eq!(identite.labels["serial"], "WKD0AB12");
        assert_eq!(identite.labels["type"], "SATA");
        assert_eq!(identite.labels["name"], "Disque 1");

        // Les séries chiffrées, elles, ne portent pas le numéro de série : un
        // remplacement de disque ne doit pas couper l'historique de la baie.
        let temperature = samples
            .iter()
            .find(|s| {
                s.metric == "synology_disk_temperature_celsius" && s.labels["disk"] == "sata1"
            })
            .unwrap();
        assert_eq!(temperature.value, 38.0);
        assert!(!temperature.labels.contains_key("serial"));
    }

    #[test]
    fn les_seuils_doccupation_viennent_du_nas_et_non_du_code() {
        let storage: StorageInfo = extraire(STORAGE);
        let samples = env_samples(storage.env.as_ref().unwrap(), 1000);

        // DSM les exprime en fraction d'espace libre : 0,2 signifie 80 % d'occupation.
        let avertissement = premiere(&samples, "synology_volume_used_warning_percent").unwrap();
        assert!((avertissement - 80.0).abs() < 1e-9, "{avertissement}");
        let critique = premiere(&samples, "synology_volume_used_critical_percent").unwrap();
        assert!((critique - 90.0).abs() < 1e-9, "{critique}");

        assert_eq!(premiere(&samples, "synology_system_crashed"), Some(0.0));
        assert_eq!(premiere(&samples, "synology_system_need_repair"), Some(0.0));
    }

    #[test]
    fn un_seuil_aberrant_annonce_par_le_nas_est_ignore() {
        let storage: StorageInfo = extraire(
            r#"{"data":{"volumes":[],"disks":[],
                "env":{"volume_full_warning":42,"volume_full_critical":-1}},"success":true}"#,
        );
        let samples = env_samples(storage.env.as_ref().unwrap(), 1000);
        assert!(samples.iter().all(|s| !s.metric.contains("_percent")));
    }

    #[test]
    fn la_sante_globale_reprend_la_pire_situation_constatee() {
        let storage: StorageInfo = extraire(STORAGE);
        assert_eq!(storage_health_sample(&storage, 1000).value, 2.0);

        let sain: StorageInfo = extraire(
            r#"{"data":{"volumes":[{"id":"volume_1","status":"normal"}],
                "disks":[{"id":"sata1","status":"normal","smart_status":"normal"}]},"success":true}"#,
        );
        assert_eq!(storage_health_sample(&sain, 1000).value, 0.0);
        assert_eq!(storage_health_sample(&sain, 1000).series_key(), "synology_storage_health");
    }

    #[test]
    fn un_systeme_declare_ecroule_par_le_nas_domine_la_sante_globale() {
        // Le verdict du NAS sur lui-même prime : il connaît des défauts que
        // l'inventaire des volumes ne montre pas.
        let storage: StorageInfo = extraire(
            r#"{"data":{"volumes":[{"id":"volume_1","status":"normal"}],"disks":[],
                "env":{"status":{"system_crashed":true,"system_need_repair":false}}},
                "success":true}"#,
        );
        assert_eq!(storage_health_sample(&storage, 1000).value, 2.0);

        let a_reparer: StorageInfo = extraire(
            r#"{"data":{"volumes":[],"disks":[],
                "env":{"status":{"system_crashed":false,"system_need_repair":true}}},
                "success":true}"#,
        );
        assert_eq!(storage_health_sample(&a_reparer, 1000).value, 1.0);
    }

    #[test]
    fn un_etat_inconnu_de_dsm_est_signale_sans_etre_declare_critique() {
        // DSM introduit de nouveaux libellés au fil des versions : les traiter comme
        // « normal » masquerait un problème, comme « critique » réveillerait pour rien.
        assert_eq!(severity("un_etat_tout_neuf"), 1.0);
        assert_eq!(severity("NORMAL"), 0.0, "la casse ne doit pas compter");
        assert_eq!(severity(" normal "), 0.0);
        assert_eq!(severity("crashed"), 2.0);
        assert_eq!(severity("background"), 1.0);
    }

    #[test]
    fn toutes_les_metriques_portent_le_prefixe_de_lintegration() {
        // Sans ce préfixe, `disk_temperature` du NAS et celui d'une sonde SNMP se
        // retrouveraient dans la même série.
        let info: SystemInfo = extraire(SYSTEM_INFO);
        let usage: Utilization = extraire(UTILIZATION);
        let storage: StorageInfo = extraire(STORAGE);

        let samples: Vec<Sample> = system_samples(&info, 1)
            .into_iter()
            .chain(utilization_samples(&usage, 1))
            .chain(volume_samples(&storage.volumes, 1))
            .chain(disk_samples(&storage.disks, 1))
            .chain(env_samples(storage.env.as_ref().unwrap(), 1))
            .chain(std::iter::once(storage_health_sample(&storage, 1)))
            .collect();

        assert!(!samples.is_empty());
        for sample in &samples {
            assert!(sample.metric.starts_with(P), "métrique sans préfixe : {}", sample.metric);
        }
    }

    #[test]
    fn aucune_etiquette_didentite_nest_posee_par_le_collecteur() {
        // Le registre pose `target`, `host` et `tag_*` : les poser ici les ferait
        // écraser, ou pire, diverger.
        let storage: StorageInfo = extraire(STORAGE);
        let samples = disk_samples(&storage.disks, 1000);
        for sample in &samples {
            for interdite in ["target", "host"] {
                assert!(
                    !sample.labels.contains_key(interdite),
                    "{} pose l'étiquette réservée {interdite}",
                    sample.metric
                );
            }
        }
    }
}
