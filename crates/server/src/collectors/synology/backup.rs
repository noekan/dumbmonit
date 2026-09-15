//! Sauvegardes Hyper Backup.
//!
//! `SYNO.Backup.Task` n'est pas documenté par Synology, mais sa forme est stable et
//! recoupée par plusieurs relevés réels (fixtures de projets tiers, sondes Zabbix et
//! Checkmk). On s'y limite à deux appels en lecture :
//!
//! * `method=list` donne l'inventaire des tâches — sans aucune date ;
//! * `method=status`, une fois par tâche, donne les dates et le résultat de la
//!   dernière exécution.
//!
//! # Le problème des dates, et pourquoi on publie une ancienneté
//!
//! DSM renvoie ses dates en texte, **à l'heure locale du NAS et sans décalage** :
//! `"2026/04/24 02:31"`. Les convertir en horodatage Unix demanderait de connaître
//! le fuseau du NAS, que l'API ne donne pas sous une forme exploitable sans base de
//! fuseaux. Publier un horodatage faux de plusieurs heures serait pire que de ne
//! rien publier.
//!
//! On publie donc une **ancienneté**, obtenue en soustrayant la date de sauvegarde
//! de l'heure courante *du NAS lui-même* (`SYNO.Core.System.info.time`). Les deux
//! étant dans le même fuseau, la différence est juste quel que soit ce fuseau —
//! et c'est précisément l'ancienneté, pas la date absolue, qui alimente la règle
//! « aucune sauvegarde réussie depuis trop longtemps ».

use chrono::NaiveDateTime;
use ezymonit_proto::{MetricKind, Sample};

use super::model::{BackupStatus, BackupTask};

/// Nombre maximal de tâches interrogées individuellement.
///
/// Chaque tâche coûte une requête : la borne évite qu'un NAS aux dizaines de
/// tâches ne fasse déborder le délai global de l'interrogation.
pub const MAX_TASKS: usize = 20;

fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("synology_{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Gravité du résultat de la dernière exécution : `0` normal, `1` à surveiller,
/// `2` échec.
///
/// `backingup` et `version_deleting` sont des états de fonctionnement normal, pas
/// des anomalies : une sauvegarde en cours au moment de l'interrogation ne doit pas
/// déclencher d'alerte. `none` signifie « jamais exécutée », ce qui mérite un
/// signalement sans être un échec.
pub fn result_severity(result: &str) -> f64 {
    match result.trim().to_ascii_lowercase().as_str() {
        "done" | "success" | "backingup" | "version_deleting" | "resuming" => 0.0,
        "none" | "cancel" | "suspend" | "partial" | "discard" | "deleting" | "unknown" => 1.0,
        "failed"
        | "cksum_failed"
        | "dest_missing"
        | "failed_checking"
        | "version_delete_failed"
        | "error"
        | "broken" => 2.0,
        // Comme pour les volumes, un libellé inconnu mérite un coup d'œil sans
        // valoir une alerte critique.
        _ => 1.0,
    }
}

/// Analyse une date de sauvegarde telle que DSM l'écrit.
///
/// Deux formes coexistent selon la version : avec et sans les secondes. Une chaîne
/// vide signifie « jamais exécutée » et ne doit pas produire de date à l'époque
/// Unix, qui se lirait comme une sauvegarde de 1970.
pub fn parse_dsm_datetime(raw: &str) -> Option<NaiveDateTime> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    ["%Y/%m/%d %H:%M:%S", "%Y/%m/%d %H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
}

/// Analyse l'heure courante du NAS, telle que `SYNO.Core.System.info` la renvoie.
///
/// C'est la référence sans laquelle aucune ancienneté n'est calculable : l'horloge
/// du serveur EzyMonit est en temps universel, celle du NAS en heure locale, et
/// rien dans la réponse ne donne l'écart entre les deux.
pub fn parse_nas_clock(raw: &str) -> Option<NaiveDateTime> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    [
        // « Sun Aug 31 14:22:03 2025 », forme de `ctime`.
        "%a %b %e %H:%M:%S %Y",
        "%a %b %d %H:%M:%S %Y",
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
    ]
    .into_iter()
    .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
}

/// Convertit l'inventaire et l'état des tâches en échantillons.
///
/// `nas_now` est l'heure locale du NAS ; quand elle manque, les métriques
/// d'ancienneté sont simplement omises plutôt que calculées à partir d'une horloge
/// dont le fuseau diffère.
pub fn task_samples(
    tasks: &[(BackupTask, Option<BackupStatus>)],
    nas_now: Option<NaiveDateTime>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = vec![gauge("backup_tasks", tasks.len() as f64, ts_ms)];

    for (task, status) in tasks {
        let id = (task.task_id.0 as i64).to_string();
        let name = task.name.clone().unwrap_or_else(|| id.clone());
        let target = task.target_type.clone().unwrap_or_default();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("task", id.clone())
                    .with_label("name", name.clone())
                    .with_label("target_type", target.clone()),
            );
        };

        // Sans état, la tâche existe mais n'a pas pu être interrogée : on publie
        // quand même son existence, sinon elle disparaîtrait des graphes.
        let Some(status) = status else {
            push(gauge("backup_last_result", 1.0, ts_ms).with_label("result", "unknown"));
            continue;
        };

        let result = status.last_bkp_result.clone().unwrap_or_else(|| "none".to_string());
        push(
            gauge("backup_last_result", result_severity(&result), ts_ms)
                .with_label("result", result.clone()),
        );
        push(gauge("backup_running", if result == "backingup" { 1.0 } else { 0.0 }, ts_ms));

        let Some(nas_now) = nas_now else { continue };

        // L'ancienneté de la dernière réussite est la métrique qui compte : une
        // tâche qui échoue depuis trois jours a un `last_bkp_time` tout frais et un
        // `last_bkp_success_time` qui, lui, ne bouge plus.
        if let Some(date) = status.last_bkp_success_time.as_deref().and_then(parse_dsm_datetime) {
            let age = (nas_now - date).num_seconds();
            push(gauge("backup_last_success_age_seconds", age.max(0) as f64, ts_ms));
        }
        if let Some(date) = status.last_bkp_time.as_deref().and_then(parse_dsm_datetime) {
            let age = (nas_now - date).num_seconds();
            push(gauge("backup_last_run_age_seconds", age.max(0) as f64, ts_ms));
        }
        if let Some(date) = status.next_bkp_time.as_deref().and_then(parse_dsm_datetime) {
            let delai = (date - nas_now).num_seconds();
            push(gauge("backup_next_run_in_seconds", delai as f64, ts_ms));
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::synology::model::{BackupTaskList, Envelope};

    /// `SYNO.Backup.Task&method=list` : deux tâches, aucune date — c'est justement
    /// pour cela qu'un appel `status` par tâche est nécessaire.
    const TASK_LIST: &str = r#"{"data":{
      "is_data_restoring":false,"is_downloading":false,"is_restoring":false,
      "task_list":[
        {"data_enc":false,"data_type":"data","name":"Sauvegarde locale","repo_id":3,
         "state":"backupable","status":"none","target_id":"nas_1.hbk","target_type":"image",
         "task_id":3,"transfer_type":"image_local","type":"image:image_local"},
        {"data_enc":false,"data_type":"data","name":"Sauvegarde distante","repo_id":4,
         "state":"backupable","status":"none","target_id":"nas_1.hbk","target_type":"image",
         "task_id":4,"transfer_type":"image_remote","type":"image:image_remote"}
      ],
      "total":2
    },"success":true}"#;

    /// `SYNO.Backup.Task&method=status` d'une tâche saine.
    const STATUS_OK: &str = r#"{"data":{
      "is_modified":false,
      "last_bkp_end_time":"2026/04/24 02:31",
      "last_bkp_error":"",
      "last_bkp_error_code":4401,
      "last_bkp_result":"done",
      "last_bkp_success_time":"2026/04/24 02:30",
      "last_bkp_success_version":"2729",
      "last_bkp_time":"2026/04/24 02:31",
      "next_bkp_time":"2026/04/25 01:00",
      "state":"backupable","status":"none","task_id":3
    },"success":true}"#;

    /// Tâche qui échoue depuis plusieurs jours : la dernière tentative est récente,
    /// la dernière réussite ne l'est plus.
    const STATUS_KO: &str = r#"{"data":{
      "last_bkp_error":"Impossible de joindre la destination",
      "last_bkp_result":"dest_missing",
      "last_bkp_success_time":"2026/04/18 02:30",
      "last_bkp_time":"2026/04/24 02:31",
      "next_bkp_time":"2026/04/25 01:00",
      "state":"backupable","status":"none","task_id":4
    },"success":true}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data.unwrap()
    }

    fn maintenant() -> NaiveDateTime {
        parse_nas_clock("Fri Apr 24 10:30:00 2026").unwrap()
    }

    fn valeur(samples: &[Sample], metric: &str, task: &str) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get("task").is_some_and(|id| id == task))
            .map(|s| s.value)
    }

    #[test]
    fn linventaire_des_taches_est_lu_tel_quel() {
        let list: BackupTaskList = extraire(TASK_LIST);
        assert_eq!(list.task_list.len(), 2);
        assert_eq!(list.task_list[0].task_id.0, 3.0);
        assert_eq!(list.task_list[0].name.as_deref(), Some("Sauvegarde locale"));
        assert_eq!(list.task_list[1].target_type.as_deref(), Some("image"));
    }

    #[test]
    fn les_deux_ecritures_de_date_de_dsm_sont_acceptees() {
        assert!(parse_dsm_datetime("2026/04/24 02:31").is_some());
        assert!(parse_dsm_datetime("2026/04/24 02:31:07").is_some());
        assert_eq!(
            parse_dsm_datetime("2026/04/24 02:31").unwrap().and_utc().timestamp(),
            parse_dsm_datetime("2026-04-24 02:31").unwrap().and_utc().timestamp()
        );
    }

    #[test]
    fn une_tache_jamais_executee_ne_produit_pas_une_date_de_1970() {
        // DSM renvoie une chaîne vide : la convertir en horodatage nul ferait
        // afficher une sauvegarde vieille de cinquante ans.
        assert_eq!(parse_dsm_datetime(""), None);
        assert_eq!(parse_dsm_datetime("   "), None);
        assert_eq!(parse_dsm_datetime("jamais"), None);
    }

    #[test]
    fn lheure_du_nas_est_lue_dans_sa_forme_habituelle() {
        let horloge = parse_nas_clock("Sun Aug 31 14:22:03 2025").unwrap();
        assert_eq!(horloge.format("%Y-%m-%d %H:%M:%S").to_string(), "2025-08-31 14:22:03");
        // Jour sur un seul chiffre : DSM complète avec une espace.
        assert!(parse_nas_clock("Sun Aug  3 14:22:03 2025").is_some());
        assert_eq!(parse_nas_clock(""), None);
    }

    #[test]
    fn une_sauvegarde_saine_est_datee_et_classee_normale() {
        let list: BackupTaskList = extraire(TASK_LIST);
        let status: BackupStatus = extraire(STATUS_OK);
        let taches = vec![(list.task_list.into_iter().next().unwrap(), Some(status))];

        let samples = task_samples(&taches, Some(maintenant()), 1000);

        assert_eq!(valeur(&samples, "synology_backup_last_result", "3"), Some(0.0));
        assert_eq!(valeur(&samples, "synology_backup_running", "3"), Some(0.0));
        // 24 avril 02:30 → 24 avril 10:30 : huit heures.
        assert_eq!(
            valeur(&samples, "synology_backup_last_success_age_seconds", "3"),
            Some(28_800.0)
        );
        // Prochaine exécution le 25 à 01:00, soit 14 h 30 plus tard.
        assert_eq!(valeur(&samples, "synology_backup_next_run_in_seconds", "3"), Some(52_200.0));
    }

    #[test]
    fn une_tache_qui_echoue_depuis_des_jours_est_reperable() {
        // Le piège : `last_bkp_time` est tout frais, seule la dernière *réussite*
        // révèle que rien n'est sauvegardé depuis six jours.
        let list: BackupTaskList = extraire(TASK_LIST);
        let status: BackupStatus = extraire(STATUS_KO);
        let tache = list.task_list.into_iter().nth(1).unwrap();
        let samples = task_samples(&[(tache, Some(status))], Some(maintenant()), 1000);

        assert_eq!(valeur(&samples, "synology_backup_last_result", "4"), Some(2.0));
        assert_eq!(
            valeur(&samples, "synology_backup_last_run_age_seconds", "4"),
            Some(28_740.0),
            "la dernière tentative date de moins de huit heures"
        );
        assert_eq!(
            valeur(&samples, "synology_backup_last_success_age_seconds", "4"),
            Some(6.0 * 86_400.0 + 28_800.0),
            "la dernière réussite remonte à six jours et huit heures"
        );
    }

    #[test]
    fn une_sauvegarde_en_cours_nest_pas_une_anomalie() {
        let list: BackupTaskList = extraire(TASK_LIST);
        let status: BackupStatus =
            extraire(r#"{"data":{"last_bkp_result":"backingup","task_id":3},"success":true}"#);
        let tache = list.task_list.into_iter().next().unwrap();
        let samples = task_samples(&[(tache, Some(status))], Some(maintenant()), 1000);

        assert_eq!(valeur(&samples, "synology_backup_last_result", "3"), Some(0.0));
        assert_eq!(valeur(&samples, "synology_backup_running", "3"), Some(1.0));
    }

    #[test]
    fn sans_horloge_du_nas_aucune_anciennete_nest_inventee() {
        // Un décalage de fuseau silencieux vaudrait pire qu'une métrique absente.
        let list: BackupTaskList = extraire(TASK_LIST);
        let status: BackupStatus = extraire(STATUS_OK);
        let tache = list.task_list.into_iter().next().unwrap();
        let samples = task_samples(&[(tache, Some(status))], None, 1000);

        assert!(samples.iter().all(|s| !s.metric.contains("age_seconds")));
        assert_eq!(valeur(&samples, "synology_backup_last_result", "3"), Some(0.0));
    }

    #[test]
    fn une_tache_dont_letat_na_pas_pu_etre_lu_reste_visible() {
        let list: BackupTaskList = extraire(TASK_LIST);
        let tache = list.task_list.into_iter().next().unwrap();
        let samples = task_samples(&[(tache, None)], Some(maintenant()), 1000);

        assert_eq!(valeur(&samples, "synology_backup_tasks", "3"), None);
        assert_eq!(samples[0].metric, "synology_backup_tasks");
        assert_eq!(samples[0].value, 1.0);
        let resultat = samples.iter().find(|s| s.metric == "synology_backup_last_result").unwrap();
        assert_eq!(resultat.value, 1.0);
        assert_eq!(resultat.labels["result"], "unknown");
    }

    #[test]
    fn les_resultats_dexecution_sont_classes_selon_leur_gravite() {
        for normal in ["done", "success", "backingup", "version_deleting", "DONE"] {
            assert_eq!(result_severity(normal), 0.0, "{normal}");
        }
        for echec in ["failed", "cksum_failed", "dest_missing", "failed_checking"] {
            assert_eq!(result_severity(echec), 2.0, "{echec}");
        }
        for attention in ["none", "cancel", "suspend", "partial", "un_libelle_tout_neuf"] {
            assert_eq!(result_severity(attention), 1.0, "{attention}");
        }
    }

    #[test]
    fn une_anciennete_negative_est_ramenee_a_zero() {
        // L'horloge du NAS peut être en retard de quelques secondes sur la date
        // qu'il vient lui-même d'écrire ; une ancienneté négative n'a pas de sens.
        let list: BackupTaskList = extraire(TASK_LIST);
        let status: BackupStatus = extraire(
            r#"{"data":{"last_bkp_result":"done","last_bkp_success_time":"2026/04/24 23:00"},
                "success":true}"#,
        );
        let tache = list.task_list.into_iter().next().unwrap();
        let samples = task_samples(&[(tache, Some(status))], Some(maintenant()), 1000);
        assert_eq!(valeur(&samples, "synology_backup_last_success_age_seconds", "3"), Some(0.0));
    }
}
