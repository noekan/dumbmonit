//! Règles livrées par défaut.
//!
//! Objectif produit : une instance fraîchement installée doit alerter utilement
//! sans que personne n'ait rien réglé. Ces règles sont insérées au premier
//! démarrage puis appartiennent à l'utilisateur — elles ne sont jamais réécrites
//! par-dessus ses modifications, seules les règles manquantes sont recréées.

use std::time::Duration;

use crate::alerting::model::{
    AnomalyParams, Operator, RULE_HOST_DOWN, Rule, RuleKind, Severity, TargetSelector,
};

/// Taux d'occupation processeur, toutes sources confondues.
///
/// Chaque collecteur nomme la sienne : `cpu_load_percent` pour SNMP
/// (HOST-RESOURCES-MIB), `proxmox_node_cpu_percent` pour Proxmox,
/// `cpu_usage_percent` pour l'agent et la démonstration. L'union `or` fait que la
/// règle livrée fonctionne quel que soit le type d'équipement ajouté, sans que
/// l'utilisateur ait à écrire quoi que ce soit.
const CPU_PERCENT: &str = "avg by (target, host) (\
     dumbmonit_cpu_load_percent \
     or dumbmonit_proxmox_node_cpu_percent \
     or dumbmonit_cpu_usage_percent)";

/// Taux de remplissage des systèmes de fichiers.
///
/// SNMP expose des octets utilisés et totaux (HOST-RESOURCES-MIB), pas un
/// pourcentage : il est calculé ici. Proxmox fournit déjà le taux.
const FS_USED_PERCENT: &str = "(\
     100 * dumbmonit_storage_bytes_used / dumbmonit_storage_bytes_total \
     or dumbmonit_proxmox_storage_used_percent \
     or dumbmonit_proxmox_node_rootfs_percent)";

/// Rappel par défaut : six heures. Assez pour ne pas oublier une panne en cours,
/// assez peu pour ne pas devenir du bruit pendant une semaine de vacances.
const DEFAULT_REPEAT: Duration = Duration::from_secs(6 * 3600);

fn base(uid: &str, name: &str, kind: RuleKind, query: &str) -> Rule {
    Rule {
        // `id = 0` : la règle n'est pas encore en base, l'insertion l'attribuera.
        id: 0,
        uid: uid.to_string(),
        name: name.to_string(),
        description: String::new(),
        kind,
        query: query.to_string(),
        operator: Operator::Gt,
        threshold: 0.0,
        // Pas d'hystérésis par défaut : chaque règle choisit son seuil de retour.
        clear_threshold: None,
        for_duration: Duration::ZERO,
        severity: Severity::Warning,
        selector: TargetSelector::All,
        // Vide signifie « tous les canaux actifs » : sans cela, les règles livrées
        // seraient muettes tant que l'utilisateur n'aurait pas pensé à les rattacher
        // au canal qu'il vient de créer.
        channels: Vec::new(),
        params: AnomalyParams::default(),
        unit: String::new(),
        repeat_interval: Some(DEFAULT_REPEAT),
        escalate_after: None,
        enabled: true,
        builtin: true,
    }
}

/// Les règles livrées avec le produit.
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        // Équipement injoignable.
        //
        // On ne cherche pas un `up == 0` : le planificateur n'écrit rien quand
        // l'interrogation échoue, la série s'arrête simplement. On mesure donc l'âge
        // du dernier échantillon, ce qui détecte aussi bien une panne réseau qu'un
        // collecteur bloqué. La fenêtre de sept jours borne le coût de la requête ;
        // au-delà, la cible n'a plus de série du tout et l'interface prend le relais.
        Rule {
            description: "No measurement received for more than three minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 180.0,
            for_duration: Duration::from_secs(60),
            severity: Severity::Critical,
            unit: "s".to_string(),
            ..base(
                RULE_HOST_DOWN,
                "Device unreachable",
                RuleKind::Threshold,
                "time() - tlast_over_time(dumbmonit_up[7d])",
            )
        },
        // Processeur élevé. L'agrégation ramène les cœurs à une seule série par
        // équipement : personne ne veut vingt-quatre alertes pour un seul serveur.
        Rule {
            description: "CPU load sustained above 90%.".to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            // Hystérésis : une charge qui oscille entre 88 et 92 % ne doit pas
            // déclencher et résoudre à chaque cycle.
            clear_threshold: Some(85.0),
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            escalate_after: Some(Duration::from_secs(3600)),
            ..base("cpu_high", "High CPU", RuleKind::Threshold, CPU_PERCENT)
        },
        // Disque presque plein, par point de montage : ici on veut bien une série
        // par système de fichiers, la remédiation n'étant pas la même.
        Rule {
            description: "Filesystem 90% full or more.".to_string(),
            operator: Operator::Ge,
            threshold: 90.0,
            clear_threshold: Some(88.0),
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            escalate_after: Some(Duration::from_secs(24 * 3600)),
            ..base("disk_almost_full", "Disk almost full", RuleKind::Threshold, FS_USED_PERCENT)
        },
        // Onduleur sur batterie : `for` très court, la coupure est déjà l'événement.
        Rule {
            description: "The UPS is powering the load from battery.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(30),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(900)),
            ..base(
                "ups_on_battery",
                "UPS on battery",
                RuleKind::Threshold,
                // upsOutputSource : 5 = batterie (RFC 1628). La comparaison rend une
                // série valant 1 seulement quand la condition est vraie, ce qui donne
                // bien « supérieur à 0 » comme test de déclenchement.
                "dumbmonit_ups_output_source == 5",
            )
        },
        // Prédictif : tout le calcul est fait par VictoriaMetrics. Le `and deriv(...)`
        // écarte les systèmes de fichiers stables ou en décroissance, pour lesquels
        // l'extrapolation linéaire n'a aucun sens.
        Rule {
            description: "At this rate, the filesystem will be full within four days.".to_string(),
            operator: Operator::Ge,
            threshold: 100.0,
            for_duration: Duration::from_secs(30 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            ..base(
                "fs_will_be_full",
                "Filesystem almost full",
                RuleKind::Predict,
                &predict_full_query(FS_USED_PERCENT, 6, 4),
            )
        },
        // Anomalie saisonnière sur le processeur. Le seuil et la durée sont ceux du
        // jalon 5 ; la règle reste muette pendant les quatorze jours d'apprentissage.
        Rule {
            description: "CPU load noticeably different from the usual at this time \
                          and day of the week."
                .to_string(),
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Info,
            unit: "%".to_string(),
            ..base("cpu_anomaly", "Unusual CPU", RuleKind::Anomaly, CPU_PERCENT)
        },
        // Batterie d'onduleur en fin de vie ou déchargée. Distincte de « sur
        // batterie » : une coupure brève est normale, une batterie basse annonce un
        // arrêt brutal des équipements alimentés.
        Rule {
            description: "The UPS battery is low or depleted.".to_string(),
            operator: Operator::Ge,
            threshold: 3.0,
            for_duration: Duration::from_secs(60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(1800)),
            ..base(
                "ups_battery_low",
                "UPS battery low",
                RuleKind::Threshold,
                // upsBatteryStatus : 3 = basse, 4 = épuisée (RFC 1628).
                "dumbmonit_ups_battery_status",
            )
        },
        // Sauvegarde Proxmox trop ancienne. C'est le genre de panne silencieuse qui
        // ne se découvre qu'au moment de restaurer, quand il est trop tard.
        Rule {
            description: "No successful backup for more than seven days.".to_string(),
            operator: Operator::Gt,
            threshold: 7.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "backup_too_old",
                "Backup too old",
                RuleKind::Threshold,
                "dumbmonit_proxmox_backup_last_age_seconds",
            )
        },
        // Proxmox Backup Server. Les seuils sont ceux d'un homelab qui sauvegarde
        // chaque nuit : deux jours sans sauvegarde, c'est une nuit ratée plus la
        // marge d'une nuit ; huit jours sans GC, c'est une GC hebdomadaire manquée.
        Rule {
            description: "PBS datastore more than 90% full.".to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            escalate_after: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_datastore_almost_full",
                "PBS datastore almost full",
                RuleKind::Threshold,
                "dumbmonit_pbs_datastore_used_percent",
            )
        },
        // L'estimation est calculée par PBS lui-même sur un mois de mesures : elle
        // est absente tant qu'il manque de points, la règle reste alors muette.
        Rule {
            description: "At the current rate, PBS estimates the datastore full within seven days."
                .to_string(),
            operator: Operator::Lt,
            threshold: 7.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_datastore_will_be_full",
                "PBS datastore filling up",
                RuleKind::Threshold,
                "dumbmonit_pbs_datastore_estimated_full_seconds",
            )
        },
        // Même logique que `backup_too_old` côté PVE, par groupe de sauvegarde.
        Rule {
            description: "No new snapshot for this machine for more than two days.".to_string(),
            operator: Operator::Gt,
            threshold: 2.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_backup_too_old",
                "PBS backup too old",
                RuleKind::Threshold,
                "dumbmonit_pbs_backup_last_age_seconds",
            )
        },
        // Critique : une sauvegarde dont la vérification échoue est une sauvegarde
        // que l'on ne pourra pas restaurer. La série vaut 1 (vérifiée) ou 0 (en
        // échec) et n'existe pas pour un instantané jamais vérifié : « < 1 » ne vise
        // donc que les vrais échecs. On ne filtre pas par `== 0` côté MetricsQL :
        // la série renvoyée garderait sa valeur 0, et aucun seuil « > 0 » ne la
        // verrait.
        Rule {
            description: "Verification of the latest snapshot for this machine failed.".to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(30 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_backup_verification_failed",
                "PBS backup verification failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_backup_last_verified",
            )
        },
        Rule {
            description: "At least one PBS task failed in the review window.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_task_failed",
                "PBS task failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_tasks_failed",
            )
        },
        // Sans GC, les blocs des instantanés supprimés ne sont jamais libérés et
        // le datastore se remplit sans raison apparente.
        Rule {
            description:
                "No successful garbage collection (GC) on this datastore for more than eight days."
                    .to_string(),
            operator: Operator::Gt,
            threshold: 8.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_gc_too_old",
                "PBS garbage collection too old",
                RuleKind::Threshold,
                "dumbmonit_pbs_gc_last_success_age_seconds",
            )
        },
        // Moniteurs de disponibilité. Contrairement au matériel, un service en
        // panne écrit bien un point (`probe_success = 0`) : la règle est un simple
        // seuil, et le `for` absorbe un raté isolé. `== bool 0` rend 1 pour un
        // service en panne ; sans `bool`, la comparaison renverrait 0 (la valeur
        // de gauche) et le seuil « > 0 » ne se déclencherait jamais.
        //
        // Les heartbeats (`probe="push"`) écrivent la même série mais ont leur
        // propre règle, `push_missed`, en avertissement : un cron en retard n'est
        // pas une panne critique, et il ne doit pas alerter deux fois.
        Rule {
            description: "The service has not responded correctly for three minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(3 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(1800)),
            ..base(
                "service_down",
                "Service down",
                RuleKind::Threshold,
                "dumbmonit_probe_success{probe!=\"push\"} == bool 0",
            )
        },
        // Heartbeat manqué. Le collecteur `push` écrit `probe_success = 0` dès que
        // le dernier appel dépasse la période attendue plus la tolérance, ou que
        // le script a lui-même signalé `status=down` ; la règle n'a donc aucun
        // délai à connaître — il est propre à chaque cible. Le `for` court
        // n'absorbe que le battement du planificateur : la tolérance est déjà
        // dans le verdict.
        Rule {
            description: "The job has not called in within its expected interval plus grace period, or reported a failure itself."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(2 * 60),
            severity: Severity::Warning,
            ..base(
                "push_missed",
                "Heartbeat missed",
                RuleKind::Threshold,
                "dumbmonit_probe_success{probe=\"push\"} == bool 0",
            )
        },
        // Instabilité : un service qui alterne sans cesse n'est jamais « en panne »
        // assez longtemps pour la règle précédente, et pourtant il est inutilisable.
        Rule {
            description: "The service changed state more than six times in thirty minutes."
                .to_string(),
            operator: Operator::Gt,
            threshold: 6.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(3600)),
            ..base(
                "service_flapping",
                "Service flapping",
                RuleKind::Threshold,
                "changes(dumbmonit_probe_success{probe!=\"push\"}[30m])",
            )
        },
        // Lenteur : le défaut de délai des sondes est de 5 s, au-delà elles échouent
        // déjà ; 3 s soutenues pendant dix minutes est un service qui souffre.
        Rule {
            description: "The service takes more than three seconds to respond.".to_string(),
            operator: Operator::Gt,
            threshold: 3.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            unit: "s".to_string(),
            ..base(
                "service_slow",
                "Slow service",
                RuleKind::Threshold,
                "dumbmonit_probe_duration_seconds",
            )
        },
        // Certificats. Les deux règles sont disjointes (`>= 0` / `< 0`) pour qu'un
        // certificat périmé ne déclenche pas aussi « bientôt expiré ».
        Rule {
            description: "The certificate expires in less than fourteen days.".to_string(),
            operator: Operator::Lt,
            threshold: 14.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "d".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "tls_cert_expiring",
                "Certificate expiring soon",
                RuleKind::Threshold,
                "dumbmonit_probe_ssl_cert_expiry_days >= 0",
            )
        },
        Rule {
            description: "The certificate has expired.".to_string(),
            operator: Operator::Lt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            unit: "d".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "tls_cert_expired",
                "Certificate expired",
                RuleKind::Threshold,
                "dumbmonit_probe_ssl_cert_expiry_days < 0",
            )
        },
        // Conteneurs Docker, vus par l'agent. Chaque série porte le nom du
        // conteneur : l'alerte le montre, et une politique peut y répondre.
        Rule {
            description: "The container has been stopped for two minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(2 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(3600)),
            ..base(
                "container_stopped",
                "Container stopped",
                RuleKind::Threshold,
                // `== bool` rend 1 quand le conteneur est arrêté, 0 sinon ; sans
                // `bool`, MetricsQL renverrait la valeur de gauche — 0 — qu'un
                // seuil « > 0 » ne verrait jamais. Voir `service_down`.
                "dumbmonit_container_up == bool 0",
            )
        },
        Rule {
            description: "The container's health check has been failing for three minutes."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(3 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(3600)),
            ..base(
                "container_unhealthy",
                "Container unhealthy",
                RuleKind::Threshold,
                // 2 = unhealthy (0 none, 1 healthy, 3 starting).
                "dumbmonit_container_health == 2",
            )
        },
        // Un conteneur qui redémarre en boucle est rarement « arrêté » assez
        // longtemps pour la règle précédente.
        Rule {
            description: "The container restarted three times or more in fifteen minutes."
                .to_string(),
            operator: Operator::Ge,
            threshold: 3.0,
            for_duration: Duration::from_secs(60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(3600)),
            ..base(
                "container_restarting",
                "Container restarting",
                RuleKind::Threshold,
                "increase(dumbmonit_container_restart_count[15m])",
            )
        },
        // Information, pas panne : une image plus récente existe dans le dépôt.
        // Une heure de `for` absorbe une vérification passagèrement fausse.
        Rule {
            description: "A newer image is available in the registry for this container."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Info,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "container_update_available",
                "Container update available",
                RuleKind::Threshold,
                "dumbmonit_container_update_available == 1",
            )
        },
        // Sauvegardes Plakar, par kloset et par source. Mêmes seuils que PBS :
        // deux jours, c'est une nuit ratée plus la marge d'une nuit. Les deux
        // règles ne visent que des séries étiquetées `kloset`, que l'agent
        // n'émet qu'une fois Plakar détecté : sans Plakar, rien ne peut se
        // déclencher.
        Rule {
            description: "No new Plakar snapshot for this source for more than two days."
                .to_string(),
            operator: Operator::Gt,
            threshold: 2.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "plakar_backup_too_old",
                "Plakar backup too old",
                RuleKind::Threshold,
                "dumbmonit_backup_last_success_seconds",
            )
        },
        // La série vaut 1 (dernier instantané sans erreur) ou 0 (erreurs, ou
        // kloset illisible) : « < 1 » ne vise que les vrais échecs.
        Rule {
            description: "The latest Plakar snapshot has errors, or the kloset cannot be read."
                .to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "plakar_backup_failed",
                "Plakar backup failed",
                RuleKind::Threshold,
                "dumbmonit_backup_last_status",
            )
        },
        // Active Backup for Business (`collectors/synology/abb.rs`). `last_status`
        // vaut 1 réussite, 0 échec, 2 en cours, -1 inconnu : « == bool 0 » rend une
        // série valant 1 pour chaque tâche en échec et 0 pour les autres, ce qu'un
        // seuil « > 0 » lit sans ambiguïté — un « < 1 » attraperait aussi l'inconnu.
        Rule {
            description: "The last run of this Active Backup for Business task failed, \
                          or backed up only part of its devices."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_abb_task_failed",
                "Active Backup task failed",
                RuleKind::Threshold,
                "dumbmonit_abb_task_last_status == bool 0",
            )
        },
        // Mêmes seuils que les autres sauvegardes : deux jours, c'est une nuit ratée
        // plus la marge d'une nuit. La série n'existe pas pour une tâche qui n'a
        // jamais réussi ; c'est alors `synology_abb_task_failed` qui parle.
        Rule {
            description: "No successful Active Backup for Business run for this task for more \
                          than two days."
                .to_string(),
            operator: Operator::Gt,
            threshold: 2.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_abb_backup_too_old",
                "Active Backup too old",
                RuleKind::Threshold,
                "dumbmonit_abb_task_last_success_seconds",
            )
        },
        // Information : une tâche sans planning ne sauvegardera plus rien tant que
        // personne ne la lance à la main. Une heure de `for` absorbe une
        // reprogrammation en cours.
        Rule {
            description: "This Active Backup for Business task has no schedule, or its \
                          continuous backup is paused."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Info,
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "synology_abb_task_disabled",
                "Active Backup task disabled",
                RuleKind::Threshold,
                "dumbmonit_abb_task_enabled == bool 0",
            )
        },
        // Proxmox VE, au-delà des sauvegardes. Sévérités : « warning » de la
        // ligne de produit = `Critical` ici, « advisory » = `Warning`.
        //
        // Machine arrêtée alors qu'elle tournait. `1 - running` vaut 1 pour une
        // machine arrêtée ; le `and` ne retient que celles vues en marche dans les
        // deux dernières heures, pour ne pas alerter sur une machine arrêtée de
        // longue date. Une comparaison MetricsQL renvoie la valeur de gauche, pas
        // un booléen : `running == 0` donnerait 0, invisible d'un seuil « > 0 ».
        Rule {
            description: "The VM or container was running and has been stopped for five minutes."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_guest_stopped",
                "VM or container stopped",
                RuleKind::Threshold,
                "(1 - dumbmonit_proxmox_guest_running) \
                 and (max_over_time(dumbmonit_proxmox_guest_running[2h]) == 1)",
            )
        },
        // Haute disponibilité : une ressource en `error` ou `fence` ne redémarrera
        // pas toute seule, c'est une intervention.
        Rule {
            description: "A high-availability resource is in error or fenced.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(2 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(3600)),
            ..base(
                "pve_ha_resource_error",
                "HA resource in error",
                RuleKind::Threshold,
                "dumbmonit_proxmox_ha_resource_error",
            )
        },
        // Quorum perdu : plus aucune machine ne peut démarrer sur le cluster, et la
        // HA arrête celles qui tournent.
        Rule {
            description: "The cluster has lost quorum: guests can no longer be started."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(1800)),
            ..base(
                "pve_cluster_no_quorum",
                "Cluster lost quorum",
                RuleKind::Threshold,
                "1 - dumbmonit_proxmox_cluster_quorate",
            )
        },
        // Nœud hors ligne vu du cluster. Distinct de « équipement injoignable » :
        // l'API répond, c'est un membre qui manque.
        Rule {
            description: "A cluster node has been offline for two minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(2 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(1800)),
            ..base(
                "pve_node_offline",
                "Proxmox node offline",
                RuleKind::Threshold,
                "1 - dumbmonit_proxmox_node_up",
            )
        },
        // Stockage à 85 % : un cran avant « Disk almost full » (90 %), qui reste
        // l'alerte sérieuse. Un LVM-thin plein bloque toutes les machines qu'il
        // héberge, il vaut mieux prévenir tôt.
        Rule {
            description: "A Proxmox storage is more than 85% full.".to_string(),
            operator: Operator::Gt,
            threshold: 85.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_storage_almost_full",
                "Proxmox storage almost full",
                RuleKind::Threshold,
                "dumbmonit_proxmox_storage_used_percent",
            )
        },
        // Dernier travail de sauvegarde du nœud en échec (tâches `vzdump`).
        Rule {
            description: "The last backup job on this node failed.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_backup_job_failed",
                "Backup job failed",
                RuleKind::Threshold,
                "1 - dumbmonit_proxmox_backup_job_last_ok",
            )
        },
        // Instantané oublié : il grossit avec le temps et ralentit la machine, et
        // personne ne s'en souvient. Information, pas panne.
        Rule {
            description: "The oldest snapshot of this machine is more than thirty days old."
                .to_string(),
            operator: Operator::Gt,
            threshold: 30.0 * 24.0 * 3600.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Info,
            unit: "s".to_string(),
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pve_snapshot_old",
                "Old snapshot",
                RuleKind::Threshold,
                "dumbmonit_proxmox_guest_snapshot_oldest_age_seconds",
            )
        },
        // Réplication en échec : la copie de secours de la machine n'est plus à
        // jour, la bascule restaurerait un état ancien.
        Rule {
            description: "A replication job is failing: the standby copy is stale.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_replication_failed",
                "Replication failed",
                RuleKind::Threshold,
                "dumbmonit_proxmox_replication_job_error",
            )
        },
        // Ceph : 0 OK, 1 WARN, 2 ERR. Les deux règles sont disjointes (`>= 2` et
        // `== 1`) pour qu'un HEALTH_ERR ne déclenche pas aussi l'avertissement.
        Rule {
            description: "Ceph reports HEALTH_ERR: data may be unavailable.".to_string(),
            operator: Operator::Ge,
            threshold: 2.0,
            for_duration: Duration::from_secs(2 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(1800)),
            ..base(
                "pve_ceph_health_error",
                "Ceph health error",
                RuleKind::Threshold,
                "dumbmonit_proxmox_ceph_health",
            )
        },
        Rule {
            description: "Ceph reports HEALTH_WARN for more than fifteen minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_ceph_health_warning",
                "Ceph health warning",
                RuleKind::Threshold,
                "dumbmonit_proxmox_ceph_health == 1",
            )
        },
        // Mises à jour en attente : information, une fois par semaine suffit.
        Rule {
            description: "More than twenty package updates are pending on this node.".to_string(),
            operator: Operator::Gt,
            threshold: 20.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Info,
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pve_updates_pending",
                "Proxmox updates pending",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_updates_pending",
            )
        },
        // Certificat de l'interface d'un nœud : PVE renouvelle les siens lui-même,
        // pas ceux importés à la main.
        Rule {
            description: "A node certificate expires in less than fourteen days.".to_string(),
            operator: Operator::Lt,
            threshold: 14.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "d".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_certificate_expiring",
                "Node certificate expiring",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_certificate_expiry_days",
            )
        },
        // Proxmox Backup Server : synchronisation vers un site distant en échec.
        // La série vaut 1 (dernier passage réussi) ou 0 ; « < 1 » ne vise que les
        // échecs, comme pour la vérification.
        Rule {
            description: "The last run of this PBS sync job failed.".to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_sync_failed",
                "PBS sync job failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_sync_job_last_ok",
            )
        },
        Rule {
            description: "More than twenty package updates are pending on the backup server."
                .to_string(),
            operator: Operator::Gt,
            threshold: 20.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Info,
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pbs_updates_pending",
                "PBS updates pending",
                RuleKind::Threshold,
                "dumbmonit_pbs_node_updates_pending",
            )
        },
        // --- Proxmox Backup Server : travaux, GC, disques (`collectors/pbs`) ---
        //
        // Un travail de purge ou de vérification en échec n'empêche pas les
        // sauvegardes de tourner : c'est justement pourquoi personne ne le voit
        // avant que le datastore soit plein ou qu'une restauration échoue. Les
        // séries `job_last_ok` valent 1 ou 0 et n'existent pas pour un travail
        // qui n'a jamais tourné ; « < 1 » ne vise donc que les vrais échecs. Le
        // `for` de dix minutes absorbe une lecture prise pendant la relance.
        Rule {
            description: "The last run of this PBS prune job failed: nothing is pruned and the \
                          datastore keeps filling up."
                .to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_prune_failed",
                "PBS prune job failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_job_last_ok{kind=\"prune\"}",
            )
        },
        Rule {
            description: "The last run of this PBS verification job failed: at least one \
                          snapshot could not be verified."
                .to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_verify_job_failed",
                "PBS verification job failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_job_last_ok{kind=\"verify\"}",
            )
        },
        // `gc_last_run_ok` vient de `/gc` (PBS 3.3+) ou, avant, de la GC la plus
        // récente de la fenêtre de tâches.
        Rule {
            description: "The last garbage collection on this PBS datastore failed: freed \
                          space is not reclaimed."
                .to_string(),
            operator: Operator::Lt,
            threshold: 1.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_gc_failed",
                "PBS garbage collection failed",
                RuleKind::Threshold,
                "dumbmonit_pbs_gc_last_run_ok",
            )
        },
        // Disques du serveur de sauvegarde, mêmes séries que pour un nœud PVE :
        // un SMART en échec est le dernier avertissement avant de perdre le
        // datastore avec les sauvegardes qu'il porte. La série n'existe que si
        // SMART a rendu un verdict.
        Rule {
            description: "A disk of the backup server reports SMART health FAILED.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pbs_disk_smart_failed",
                "PBS disk SMART failure",
                RuleKind::Threshold,
                "dumbmonit_pbs_node_disk_smart_failed",
            )
        },
        Rule {
            description: "This SSD of the backup server has used more than 90% of its rated \
                          endurance."
                .to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pbs_disk_wearout",
                "PBS SSD worn out",
                RuleKind::Threshold,
                "dumbmonit_pbs_node_disk_wearout_percent",
            )
        },
        // `zfs_pool_degraded` vaut 0 pour ONLINE ; DEGRADED, FAULTED, OFFLINE
        // ou UNAVAIL donnent 1. Un pool dégradé fonctionne encore : c'est le
        // moment de remplacer le disque, pas après le second.
        Rule {
            description: "A ZFS pool of the backup server is not ONLINE (degraded, faulted or \
                          unavailable)."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pbs_zpool_degraded",
                "PBS ZFS pool degraded",
                RuleKind::Threshold,
                "dumbmonit_pbs_node_zfs_pool_degraded",
            )
        },
        // --- fin du bloc Proxmox Backup Server ---
        // --- Proxmox VE : invités, disques, ZFS, paquets (`collectors/proxmox`) ---
        //
        // Les séries d'invité portent `name` et `vmid` : la notification dit
        // « nextcloud (202) ». Celles de disque portent `node` et `disk`, celles
        // de pool `node` et `pool` : « pve1 · /dev/sda », « pve2 · tank ».
        //
        // Processeur d'un invité : le pourcentage est celui des cœurs alloués,
        // une machine à 100 % n'a que ses propres cœurs à saturer. Quinze
        // minutes écartent les pics de démarrage et de sauvegarde.
        Rule {
            description:
                "A VM or container has used more than 90% of its allocated cores for fifteen minutes."
                    .to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_guest_cpu_high",
                "VM or container CPU high",
                RuleKind::Threshold,
                "dumbmonit_proxmox_guest_cpu_percent",
            )
        },
        // Mémoire d'un invité : PVE mesure la mémoire consommée vue de l'hôte,
        // qui plafonne naturellement près de 100 % avec un ballon ou un cache
        // de fichiers actif — d'où 95 % et dix minutes.
        Rule {
            description: "A VM or container has used more than 95% of its memory for ten minutes."
                .to_string(),
            operator: Operator::Gt,
            threshold: 95.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_guest_memory_high",
                "VM or container memory high",
                RuleKind::Threshold,
                "dumbmonit_proxmox_guest_memory_percent",
            )
        },
        // Disque racine d'un invité : connu pour un conteneur, et pour une
        // machine virtuelle dont l'agent QEMU répond. Sans mesure, pas de série
        // et donc pas d'alerte — jamais un faux « 0 % ».
        Rule {
            description: "The root disk of a VM or container is more than 90% full.".to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_guest_disk_almost_full",
                "VM or container disk almost full",
                RuleKind::Threshold,
                "dumbmonit_proxmox_guest_disk_used_percent",
            )
        },
        // Usure d'un SSD ou NVMe : 0 % neuf, 100 % en fin de vie d'après le
        // constructeur. À 90 %, il est temps de commander le remplaçant.
        Rule {
            description: "An SSD or NVMe disk of a node has used more than 90% of its rated life."
                .to_string(),
            operator: Operator::Gt,
            threshold: 90.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pve_disk_wearout",
                "Proxmox disk wearing out",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_disk_wearout_percent",
            )
        },
        // SMART en échec : le disque lui-même se déclare en fin de vie.
        Rule {
            description: "A disk of a Proxmox node reports a failed SMART health check.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_disk_smart_failed",
                "Proxmox disk SMART failure",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_disk_smart_failed",
            )
        },
        // Pool ZFS dégradé : un disque manque, la redondance est consommée.
        Rule {
            description: "A ZFS pool of a Proxmox node is not ONLINE: a disk is missing or faulted."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "pve_zfs_pool_degraded",
                "Proxmox ZFS pool degraded",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_zfs_pool_degraded",
            )
        },
        // Correctifs de sécurité : un seul suffit, contrairement au décompte
        // général des mises à jour qui attend vingt paquets.
        Rule {
            description: "Security updates are pending on this Proxmox node.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "pve_security_updates_pending",
                "Proxmox security updates pending",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_updates_security_pending",
            )
        },
        // Paquets mis à jour : quelqu'un a passé `apt upgrade` sur le nœud. Une
        // information, pas une panne — mais celle qui explique le redémarrage
        // ou le comportement changé qu'on constate ensuite. La série reste
        // publiée une heure avec le détail en étiquette `changes`, puis se
        // résout seule.
        Rule {
            description: "Installed Proxmox packages changed since the previous probe.".to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(60),
            severity: Severity::Info,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "pve_packages_changed",
                "Proxmox packages changed",
                RuleKind::Threshold,
                "dumbmonit_proxmox_node_packages_changed",
            )
        },
        // --- fin du bloc Proxmox VE : invités, disques, ZFS, paquets ---
        // --- Synology DSM : stockage, disques, mémoire et rythme Active Backup
        // (`collectors/synology/{metrics,devices,rhythm}.rs`) ---
        //
        // Les états de DSM sont publiés sur une échelle commune : 0 normal, 1 à
        // surveiller, 2 critique — et l'inconnu vaut 1, jamais 0. Une règle sur
        // « > 0 » attrape donc aussi un état inédit, ce qui est voulu : mieux vaut
        // un avertissement qu'un disque en panne sous un libellé nouveau.
        Rule {
            description: "The SMART status of this disk is no longer \"normal\", as reported \
                          by DSM."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_disk_smart_warning",
                "Synology disk SMART warning",
                RuleKind::Threshold,
                "dumbmonit_synology_disk_smart_status",
            )
        },
        // Un disque « crashed » a déjà quitté la grappe : c'est une panne, pas un
        // avertissement.
        Rule {
            description: "DSM reports this disk as crashed or failed.".to_string(),
            operator: Operator::Ge,
            threshold: 2.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "synology_disk_failed",
                "Synology disk failed",
                RuleKind::Threshold,
                "dumbmonit_synology_disk_status",
            )
        },
        Rule {
            description: "The bad-sector count of this disk exceeds the threshold set in DSM."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_disk_bad_sectors",
                "Synology disk bad sectors",
                RuleKind::Threshold,
                "dumbmonit_synology_disk_bad_sector_exceeded",
            )
        },
        // Des secteurs illisibles qui apparaissent sont le signe le plus précoce
        // d'une panne : `delta` sur une jauge compare la valeur d'il y a un jour à
        // celle d'aujourd'hui. Un disque remplacé repart de zéro, ce qui donne une
        // variation négative et ne déclenche pas.
        Rule {
            description: "New unreadable (UNC) sectors appeared on this disk in the last \
                          24 hours."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_disk_bad_sectors_growing",
                "Synology disk bad sectors growing",
                RuleKind::Threshold,
                "delta(dumbmonit_synology_disk_unc_count[24h])",
            )
        },
        Rule {
            description: "This SSD has less than 10% of its rated life left.".to_string(),
            operator: Operator::Lt,
            threshold: 10.0,
            clear_threshold: Some(12.0),
            for_duration: Duration::from_secs(3600),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(7 * 24 * 3600)),
            ..base(
                "synology_ssd_wearout",
                "Synology SSD wearing out",
                RuleKind::Threshold,
                "dumbmonit_synology_disk_remaining_life_percent",
            )
        },
        // Un volume dégradé fonctionne encore, sans redondance : la prochaine
        // panne est celle qui perd les données. Même seuil pour « crashed ».
        Rule {
            description: "This volume is degraded or crashed: its redundancy is gone.".to_string(),
            operator: Operator::Ge,
            threshold: 2.0,
            for_duration: Duration::from_secs(5 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "synology_volume_degraded",
                "Synology volume degraded",
                RuleKind::Threshold,
                "dumbmonit_synology_volume_status",
            )
        },
        Rule {
            description: "Volume 90% full or more.".to_string(),
            operator: Operator::Ge,
            threshold: 90.0,
            clear_threshold: Some(88.0),
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            escalate_after: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_volume_almost_full",
                "Synology volume almost full",
                RuleKind::Threshold,
                "dumbmonit_synology_volume_used_percent",
            )
        },
        // 55 °C est le seuil à partir duquel les fabricants de disques mécaniques
        // et Synology parlent de surchauffe ; l'hystérésis évite les allers-retours
        // d'un disque qui oscille autour du seuil pendant une reconstruction.
        Rule {
            description: "A disk of this NAS is above 55 °C, or DSM raised its temperature \
                          warning."
                .to_string(),
            operator: Operator::Gt,
            threshold: 55.0,
            clear_threshold: Some(52.0),
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "°C".to_string(),
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "synology_temperature_high",
                "Synology temperature high",
                RuleKind::Threshold,
                // Le drapeau de DSM vaut 0 ou 1 : multiplié par 100, il dépasse le
                // seuil à lui seul quand le NAS lui-même se déclare en surchauffe.
                "dumbmonit_synology_disk_temperature_celsius \
                 or (100 * dumbmonit_synology_temperature_warning > 55)",
            )
        },
        Rule {
            description: "Memory usage of the NAS above 95% for fifteen minutes.".to_string(),
            operator: Operator::Gt,
            threshold: 95.0,
            clear_threshold: Some(90.0),
            for_duration: Duration::from_secs(15 * 60),
            severity: Severity::Warning,
            unit: "%".to_string(),
            repeat_interval: Some(Duration::from_secs(6 * 3600)),
            ..base(
                "synology_memory_high",
                "Synology memory high",
                RuleKind::Threshold,
                "dumbmonit_synology_memory_usage_percent",
            )
        },
        // Rythme Active Backup par appareil : la série vaut 1 seulement quand le
        // modèle juge l'appareil en retard *par rapport à ses propres habitudes*
        // (jours de repos exclus, tolérance dérivée de son intervalle habituel).
        // Une demi-heure de `for` laisse passer une relecture de l'historique.
        Rule {
            description: "No successful Active Backup for Business run for this device for \
                          longer than its usual rhythm allows (off-days excluded)."
                .to_string(),
            operator: Operator::Gt,
            threshold: 0.0,
            for_duration: Duration::from_secs(30 * 60),
            severity: Severity::Warning,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_abb_device_overdue",
                "Active Backup device overdue",
                RuleKind::Threshold,
                "dumbmonit_abb_device_overdue",
            )
        },
        // Deux tentatives échouées d'affilée : ce n'est plus un portable refermé
        // au mauvais moment, c'est un appareil qui ne se sauvegarde plus.
        Rule {
            description: "The last two or more Active Backup for Business attempts of this \
                          device failed."
                .to_string(),
            operator: Operator::Ge,
            threshold: 2.0,
            for_duration: Duration::from_secs(10 * 60),
            severity: Severity::Critical,
            repeat_interval: Some(Duration::from_secs(24 * 3600)),
            ..base(
                "synology_abb_device_failing",
                "Active Backup device failing",
                RuleKind::Threshold,
                "dumbmonit_abb_device_consecutive_failures",
            )
        },
        // --- fin du bloc Synology DSM ---
    ]
}

/// Construit une requête prédictive « ce système de fichiers sera plein dans moins
/// de `days` jours », pour l'assistant de création de règle de l'interface.
///
/// Le calcul reste intégralement côté VictoriaMetrics : rien n'est extrapolé en Rust,
/// ce qui évite d'avoir à rapatrier l'historique de chaque série à chaque cycle.
pub fn predict_full_query(metric: &str, window_hours: u32, days: u32) -> String {
    let horizon = u64::from(days) * 24 * 3600;
    format!(
        "predict_linear({metric}[{window_hours}h], {horizon}) and deriv({metric}[{window_hours}h]) > 0"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_regles_livrees_couvrent_les_cas_annonces() {
        let rules = builtin_rules();
        let uids: Vec<&str> = rules.iter().map(|r| r.uid.as_str()).collect();
        for attendu in [
            RULE_HOST_DOWN,
            "cpu_high",
            "disk_almost_full",
            "ups_on_battery",
            "ups_battery_low",
            "fs_will_be_full",
            "backup_too_old",
            "pbs_datastore_almost_full",
            "pbs_datastore_will_be_full",
            "pbs_backup_too_old",
            "pbs_backup_verification_failed",
            "pbs_task_failed",
            "pbs_gc_too_old",
            "service_down",
            "push_missed",
            "service_flapping",
            "service_slow",
            "tls_cert_expiring",
            "tls_cert_expired",
            "container_stopped",
            "container_unhealthy",
            "container_restarting",
            "container_update_available",
            "plakar_backup_too_old",
            "plakar_backup_failed",
            "synology_abb_task_failed",
            "synology_abb_backup_too_old",
            "synology_abb_task_disabled",
            "pve_guest_stopped",
            "pve_ha_resource_error",
            "pve_cluster_no_quorum",
            "pve_node_offline",
            "pve_storage_almost_full",
            "pve_backup_job_failed",
            "pve_snapshot_old",
            "pve_replication_failed",
            "pve_ceph_health_error",
            "pve_ceph_health_warning",
            "pve_updates_pending",
            "pve_certificate_expiring",
            "pbs_sync_failed",
            "pbs_updates_pending",
            "pbs_prune_failed",
            "pbs_verify_job_failed",
            "pbs_gc_failed",
            "pbs_disk_smart_failed",
            "pbs_disk_wearout",
            "pbs_zpool_degraded",
            "pve_guest_cpu_high",
            "pve_guest_memory_high",
            "pve_guest_disk_almost_full",
            "pve_disk_wearout",
            "pve_disk_smart_failed",
            // Synology DSM (`collectors/synology`).
            "synology_disk_smart_warning",
            "synology_disk_failed",
            "synology_disk_bad_sectors",
            "synology_disk_bad_sectors_growing",
            "synology_ssd_wearout",
            "synology_volume_degraded",
            "synology_volume_almost_full",
            "synology_temperature_high",
            "synology_memory_high",
            "synology_abb_device_overdue",
            "synology_abb_device_failing",
            "pve_zfs_pool_degraded",
            "pve_security_updates_pending",
            "pve_packages_changed",
        ] {
            assert!(uids.contains(&attendu), "missing built-in rule: {attendu}");
        }
    }

    /// Une règle qui vise une métrique inexistante ne déclenche jamais et ne se
    /// signale pas : elle donne l'illusion d'une surveillance en place. Ce test fige
    /// donc la correspondance avec les noms réellement produits par les collecteurs.
    #[test]
    fn les_regles_livrees_ne_visent_que_des_metriques_produites() {
        // Noms produits par les profils SNMP (`profiles/*.yaml`), les collecteurs
        // Proxmox VE et PBS et le registre, préfixe `dumbmonit_` inclus.
        const PRODUITES: &[&str] = &[
            "dumbmonit_up",
            "dumbmonit_cpu_load_percent",
            "dumbmonit_cpu_usage_percent",
            "dumbmonit_storage_bytes_used",
            "dumbmonit_storage_bytes_total",
            "dumbmonit_ups_output_source",
            "dumbmonit_ups_battery_status",
            "dumbmonit_proxmox_node_cpu_percent",
            "dumbmonit_proxmox_node_rootfs_percent",
            "dumbmonit_proxmox_storage_used_percent",
            "dumbmonit_proxmox_backup_last_age_seconds",
            // Proxmox Backup Server (`collectors/pbs/metrics.rs`, `backup.rs`).
            "dumbmonit_pbs_datastore_used_percent",
            "dumbmonit_pbs_datastore_estimated_full_seconds",
            "dumbmonit_pbs_backup_last_age_seconds",
            "dumbmonit_pbs_backup_last_verified",
            "dumbmonit_pbs_tasks_failed",
            "dumbmonit_pbs_gc_last_success_age_seconds",
            // Moniteurs de disponibilité (`collectors/uptime/outcome.rs`, `tls/mod.rs`).
            "dumbmonit_probe_success",
            "dumbmonit_probe_duration_seconds",
            "dumbmonit_probe_ssl_cert_expiry_days",
            // Agent : conteneurs Docker et sauvegardes Plakar (`crates/agent`).
            "dumbmonit_container_up",
            "dumbmonit_container_health",
            "dumbmonit_container_restart_count",
            "dumbmonit_container_update_available",
            "dumbmonit_backup_last_success_seconds",
            "dumbmonit_backup_last_status",
            // Synology Active Backup for Business (`collectors/synology/abb.rs`).
            "dumbmonit_abb_task_last_status",
            "dumbmonit_abb_task_last_success_seconds",
            "dumbmonit_abb_task_enabled",
            // Synology DSM : stockage, disques, mémoire et rythme Active Backup
            // (`collectors/synology/{metrics,devices}.rs`).
            "dumbmonit_synology_disk_smart_status",
            "dumbmonit_synology_disk_status",
            "dumbmonit_synology_disk_bad_sector_exceeded",
            "dumbmonit_synology_disk_unc_count",
            "dumbmonit_synology_disk_remaining_life_percent",
            "dumbmonit_synology_volume_status",
            "dumbmonit_synology_volume_used_percent",
            "dumbmonit_synology_disk_temperature_celsius",
            "dumbmonit_synology_temperature_warning",
            "dumbmonit_synology_memory_usage_percent",
            "dumbmonit_abb_device_overdue",
            "dumbmonit_abb_device_consecutive_failures",
            // Proxmox VE, parité avec Pulse (`collectors/proxmox/{metrics,ha,
            // snapshots,replication,ceph}.rs`).
            "dumbmonit_proxmox_guest_running",
            "dumbmonit_proxmox_ha_resource_error",
            "dumbmonit_proxmox_cluster_quorate",
            "dumbmonit_proxmox_node_up",
            "dumbmonit_proxmox_backup_job_last_ok",
            "dumbmonit_proxmox_guest_snapshot_oldest_age_seconds",
            "dumbmonit_proxmox_replication_job_error",
            "dumbmonit_proxmox_ceph_health",
            "dumbmonit_proxmox_node_updates_pending",
            "dumbmonit_proxmox_node_certificate_expiry_days",
            // PBS, travaux de synchronisation et mises à jour (`collectors/pbs/jobs.rs`).
            "dumbmonit_pbs_sync_job_last_ok",
            "dumbmonit_pbs_node_updates_pending",
            // Proxmox VE, invités, disques, ZFS et paquets
            // (`collectors/proxmox/{metrics,guest,disks,apt}.rs`).
            "dumbmonit_proxmox_guest_cpu_percent",
            "dumbmonit_proxmox_guest_memory_percent",
            "dumbmonit_proxmox_guest_disk_used_percent",
            "dumbmonit_proxmox_node_disk_wearout_percent",
            "dumbmonit_proxmox_node_disk_smart_failed",
            "dumbmonit_proxmox_node_zfs_pool_degraded",
            "dumbmonit_proxmox_node_updates_security_pending",
            "dumbmonit_proxmox_node_packages_changed",
            // PBS, travaux, GC et disques (`collectors/pbs/{jobs,backup,metrics}.rs`).
            "dumbmonit_pbs_job_last_ok",
            "dumbmonit_pbs_gc_last_run_ok",
            "dumbmonit_pbs_node_disk_smart_failed",
            "dumbmonit_pbs_node_disk_wearout_percent",
            "dumbmonit_pbs_node_zfs_pool_degraded",
        ];

        for rule in builtin_rules() {
            for mot in rule.query.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if mot.starts_with("dumbmonit_") {
                    assert!(
                        PRODUITES.contains(&mot),
                        "rule \"{}\" targets \"{mot}\", which no collector produces",
                        rule.uid
                    );
                }
            }
        }
    }

    #[test]
    fn les_identifiants_des_regles_livrees_sont_uniques() {
        let rules = builtin_rules();
        let mut uids: Vec<&str> = rules.iter().map(|r| r.uid.as_str()).collect();
        uids.sort_unstable();
        let total = uids.len();
        uids.dedup();
        assert_eq!(uids.len(), total, "two built-in rules share a uid");
    }

    #[test]
    fn les_regles_livrees_sont_actives_et_sans_canal_impose() {
        for rule in builtin_rules() {
            assert!(rule.enabled, "{} should be enabled", rule.uid);
            assert!(rule.builtin);
            assert!(rule.channels.is_empty(), "{} must not impose any channel", rule.uid);
            assert!(matches!(rule.selector, TargetSelector::All));
        }
    }

    #[test]
    fn seule_la_regle_injoignable_porte_l_uid_reserve() {
        let host_down: Vec<_> = builtin_rules().into_iter().filter(|r| r.is_host_down()).collect();
        assert_eq!(host_down.len(), 1);
        assert_eq!(host_down[0].severity, Severity::Critical);
    }

    #[test]
    fn la_requete_predictive_reste_du_metricsql() {
        let query = predict_full_query("dumbmonit_fs_used_percent", 6, 4);
        assert_eq!(
            query,
            "predict_linear(dumbmonit_fs_used_percent[6h], 345600) \
             and deriv(dumbmonit_fs_used_percent[6h]) > 0"
        );
    }

    #[test]
    fn toutes_les_regles_livrees_ont_un_for_non_nul_sauf_justification() {
        for rule in builtin_rules() {
            assert!(
                rule.for_duration > Duration::ZERO,
                "{} would fire on the first point, with no noise filter",
                rule.uid
            );
        }
    }
}
