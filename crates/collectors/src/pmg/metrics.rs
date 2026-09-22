//! Conversion des réponses de l'API en échantillons et en vues.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//! Les constantes de test sont des relevés d'une passerelle 9.1, copiés tels
//! quels.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    AptUpdate, CertificateInfo, ClamavDatabase, ClusterNode, MailStats, NodeStatus, Num, QshapeRow,
    QuarantineStatus, RecentPoint, ServiceEntry, SpamScore, SpamassassinChannel, Subscription,
    Version, VirusStat,
};
use super::view::{
    CertificateView, ClusterNodeView, MAX_QUEUE_DOMAINS, MAX_VIRUSES, MailView, QuarantineView,
    QueueDomainView, QueueView, RecentPointView, ServiceView, SignatureView, SpamScoreView,
    SubscriptionView, VirusView,
};

/// Préfixe commun à toutes les métriques de l'intégration.
///
/// Il isole l'intégration, comme `pbs_` pour le serveur de sauvegarde : sans lui,
/// `node_cpu_percent` entrerait en collision avec la même notion venue d'un
/// hyperviseur.
pub const P: &str = "pmg_";

pub fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Pourcentage d'occupation, ou `None` si le total est inconnu ou nul — mieux
/// vaut pas de point du tout qu'un 0 % trompeur.
fn percent(used: Option<Num>, total: Option<Num>) -> Option<f64> {
    let total = total?.0;
    let used = used?.0;
    (total > 0.0).then(|| used / total * 100.0)
}

// --------------------------------------------------------------------------
// Version et nœud
// --------------------------------------------------------------------------

/// `GET /version` : une série de présence portant la version en étiquette, selon
/// la convention `*_info` — la valeur ne sert à rien, les étiquettes à tout.
pub fn version_samples(version: &Version, ts_ms: i64) -> Vec<Sample> {
    vec![
        gauge("version_info", 1.0, ts_ms)
            .with_label("version", version.version.clone().unwrap_or_default())
            .with_label("release", version.release.clone().unwrap_or_default())
            .with_label("repoid", version.repoid.clone().unwrap_or_default()),
    ]
}

/// `GET /nodes/{node}/status`. Les séries portent toutes l'étiquette `node` :
/// une grappe de deux passerelles produit deux courbes distinctes.
pub fn node_samples(node: &str, status: &NodeStatus, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("node", node.to_string()));

    // La charge CPU est un ratio 0..1 ; on l'expose en pourcentage pour rester
    // homogène avec le reste de DumbMonit.
    if let Some(cpu) = status.cpu {
        push(gauge("node_cpu_percent", cpu.0 * 100.0, ts_ms));
    }
    if let Some(wait) = status.wait {
        push(gauge("node_iowait_percent", wait.0 * 100.0, ts_ms));
    }
    if let Some(info) = &status.cpuinfo
        && let Some(cpus) = info.cpus
    {
        push(gauge("node_cpu_count", cpus.0, ts_ms));
    }
    for (index, metric) in ["node_load1", "node_load5", "node_load15"].into_iter().enumerate() {
        if let Some(load) = status.loadavg.get(index) {
            push(gauge(metric, load.0, ts_ms));
        }
    }

    if let Some(memory) = &status.memory {
        if let Some(used) = memory.used {
            push(gauge("node_memory_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = memory.total {
            push(gauge("node_memory_total_bytes", total.0, ts_ms));
        }
        if let Some(value) = percent(memory.used, memory.total) {
            push(gauge("node_memory_used_percent", value, ts_ms));
        }
    }

    if let Some(swap) = &status.swap {
        if let Some(used) = swap.used {
            push(gauge("node_swap_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = swap.total {
            push(gauge("node_swap_total_bytes", total.0, ts_ms));
        }
    }

    if let Some(root) = &status.rootfs {
        if let Some(used) = root.used {
            push(gauge("node_rootfs_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = root.total {
            push(gauge("node_rootfs_total_bytes", total.0, ts_ms));
        }
        if let Some(avail) = root.avail.or(root.free) {
            push(gauge("node_rootfs_avail_bytes", avail.0, ts_ms));
        }
        if let Some(value) = percent(root.used, root.total) {
            push(gauge("node_rootfs_percent", value, ts_ms));
        }
    }

    // L'uptime est un `Gauge` : il repart de zéro à chaque redémarrage, et c'est
    // justement cette chute que l'on veut voir telle quelle.
    if let Some(uptime) = status.uptime {
        push(gauge("node_uptime_seconds", uptime.0, ts_ms));
    }

    // Écart d'horloge : une passerelle en retard fait échouer les vérifications
    // DKIM et date de travers les messages qu'elle relaie.
    if let Some(offset) = clock_offset(status, now_s) {
        push(gauge("node_clock_offset_seconds", offset, ts_ms));
    }

    // `insync` dit que la base de règles est à jour avec le reste de la grappe.
    // Une passerelle autonome répond 1.
    if let Some(insync) = status.insync {
        push(gauge("node_insync", if insync.0 != 0.0 { 1.0 } else { 0.0 }, ts_ms));
    }

    if let Some(kversion) = &status.kversion {
        push(gauge("node_kernel_info", 1.0, ts_ms).with_label("kversion", kversion.clone()));
    }

    // `pmg-api/9.1.2/42245585286a` : dans une grappe, c'est la seule façon de
    // voir le nœud que la dernière mise à jour a oublié.
    if let Some(version) = node_version(status) {
        push(gauge("node_version_info", 1.0, ts_ms).with_label("version", version));
    }

    samples
}

/// Version de l'API sur ce nœud, extraite de `pmg-api/9.1.2/42245585286a`.
///
/// `None` si le format change : mieux vaut pas de version qu'une chaîne
/// illisible en étiquette.
pub fn node_version(status: &NodeStatus) -> Option<String> {
    let raw = status.pmgversion.as_deref()?;
    let version = raw.split('/').nth(1).filter(|part| !part.is_empty())?;
    Some(version.to_string())
}

/// Écart entre l'horloge de la passerelle et la nôtre, en secondes.
///
/// `None` quand la passerelle ne donne pas son heure : une version ancienne, ou
/// un proxy inverse qui réécrit la réponse. Un zéro serait un mensonge.
pub fn clock_offset(status: &NodeStatus, now_s: i64) -> Option<f64> {
    let remote = status.time?.0;
    // Une valeur aberrante — date à zéro, horloge de 1970 — ne dit rien d'utile
    // et fausserait l'échelle du graphe.
    (remote > 1_000_000_000.0).then_some(remote - now_s as f64)
}

// --------------------------------------------------------------------------
// Files d'attente Postfix
// --------------------------------------------------------------------------

/// Bornes basses des tranches d'âge de `qshape`, en secondes.
///
/// `qshape` range les messages par âge dans des colonnes qui doublent : `5m`
/// compte ceux de moins de cinq minutes, `10m` ceux de cinq à dix, et ainsi de
/// suite. La dernière colonne, `1280m+`, n'a pas de borne haute.
const AGE_BUCKETS: [(&str, f64); 10] = [
    ("5m", 0.0),
    ("10m", 300.0),
    ("20m", 600.0),
    ("40m", 1_200.0),
    ("80m", 2_400.0),
    ("160m", 4_800.0),
    ("320m", 9_600.0),
    ("640m", 19_200.0),
    ("1280m", 38_400.0),
    ("1280m+", 76_800.0),
];

/// Ce qu'une file d'attente contient, d'après `GET /nodes/{node}/postfix/qshape`.
///
/// La première ligne porte `domain = "TOTAL"` et agrège les autres ; elle est la
/// seule source du décompte, les lignes par domaine servant la vue.
pub fn queue_view(queue: &str, rows: &[QshapeRow]) -> QueueView {
    let total_row = rows.iter().find(|row| row.is_total());
    let domains: Vec<&QshapeRow> = rows.iter().filter(|row| !row.is_total()).collect();

    // Sans ligne TOTAL — une version qui ne l'écrirait pas —, la somme des
    // domaines dit la même chose.
    let messages = total_row
        .and_then(QshapeRow::total)
        .unwrap_or_else(|| domains.iter().filter_map(|row| row.total()).sum());

    let mut top: Vec<QueueDomainView> = domains
        .iter()
        .filter_map(|row| {
            Some(QueueDomainView {
                domain: row.domain.clone()?,
                messages: row.total().unwrap_or_default(),
            })
        })
        .collect();
    top.sort_by(|a, b| {
        b.messages
            .partial_cmp(&a.messages)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.domain.cmp(&b.domain))
    });
    top.truncate(MAX_QUEUE_DOMAINS);

    QueueView {
        queue: queue.to_string(),
        messages,
        domains: domains.len() as f64,
        oldest_age_seconds: oldest_age(total_row.or(rows.first())),
        top_domains: top,
    }
}

/// Âge minimal du plus vieux message d'une file, en secondes.
///
/// `qshape` ne donne que des tranches : on retient la borne basse de la plus
/// haute tranche occupée. C'est donc un minorant, et c'est ce qui est documenté —
/// une estimation haute ferait sonner l'alerte trop tôt. `None` quand la file est
/// vide ou quand la réponse n'a aucune colonne d'âge.
pub fn oldest_age(row: Option<&QshapeRow>) -> Option<f64> {
    let row = row?;
    AGE_BUCKETS
        .iter()
        .rev()
        .find(|(name, _)| row.columns.get(*name).is_some_and(|count| count.0 > 0.0))
        .map(|(_, floor)| *floor)
}

pub fn queue_samples(view: &QueueView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        gauge("queue_messages", view.messages, ts_ms),
        gauge("queue_domains", view.domains, ts_ms),
    ];
    if let Some(age) = view.oldest_age_seconds {
        samples.push(gauge("queue_oldest_age_seconds", age, ts_ms));
    }
    samples.into_iter().map(|s| s.with_label("queue", view.queue.clone())).collect()
}

// --------------------------------------------------------------------------
// Statistiques de messagerie
// --------------------------------------------------------------------------

/// `GET /statistics/mail`, totaux du jour courant.
pub fn mail_view(stats: &MailStats) -> MailView {
    MailView {
        count_in: stats.count_in.map(|n| n.0),
        count_out: stats.count_out.map(|n| n.0),
        bytes_in: stats.bytes_in.map(|n| n.0),
        bytes_out: stats.bytes_out.map(|n| n.0),
        spam_in: stats.spamcount_in.map(|n| n.0),
        spam_out: stats.spamcount_out.map(|n| n.0),
        virus_in: stats.viruscount_in.map(|n| n.0),
        virus_out: stats.viruscount_out.map(|n| n.0),
        bounces_in: stats.bounces_in.map(|n| n.0),
        bounces_out: stats.bounces_out.map(|n| n.0),
        junk_in: stats.junk_in.map(|n| n.0),
        junk_out: stats.junk_out.map(|n| n.0),
        greylisted: stats.glcount.map(|n| n.0),
        spf_rejects: stats.spfcount.map(|n| n.0),
        rbl_rejects: stats.rbl_rejects.map(|n| n.0),
        pregreet_rejects: stats.pregreet_rejects.map(|n| n.0),
        avg_processing_seconds: stats.avptime.map(|n| n.0),
    }
}

/// Séries des totaux du jour.
///
/// Ce sont des `Gauge` et non des compteurs : PMG agrège par journée locale, la
/// valeur repart de zéro à minuit. Un `Counter` ferait croire à une remise à zéro
/// du processus et fausserait tout calcul de taux.
pub fn mail_samples(view: &MailView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |metric: &str, value: Option<f64>| {
        if let Some(value) = value {
            samples.push(gauge(metric, value, ts_ms));
        }
    };

    push("mail_count_in", view.count_in);
    push("mail_count_out", view.count_out);
    push("mail_bytes_in", view.bytes_in);
    push("mail_bytes_out", view.bytes_out);
    push("mail_spam_in", view.spam_in);
    push("mail_spam_out", view.spam_out);
    push("mail_virus_in", view.virus_in);
    push("mail_virus_out", view.virus_out);
    push("mail_bounces_in", view.bounces_in);
    push("mail_bounces_out", view.bounces_out);
    push("mail_junk_in", view.junk_in);
    push("mail_junk_out", view.junk_out);
    push("mail_greylisted", view.greylisted);
    push("mail_spf_rejects", view.spf_rejects);
    push("mail_rbl_rejects", view.rbl_rejects);
    push("mail_pregreet_rejects", view.pregreet_rejects);
    push("mail_avg_processing_seconds", view.avg_processing_seconds);

    // Part de courrier indésirable à l'entrée. Absente tant qu'aucun message
    // n'est entré : un 0 % sur une journée vide serait une bonne nouvelle
    // imaginaire.
    if let (Some(junk), Some(count)) = (view.junk_in, view.count_in)
        && count > 0.0
    {
        samples.push(gauge("mail_junk_percent", junk / count * 100.0, ts_ms));
    }

    samples
}

/// `GET /statistics/recent` : la courbe des dernières heures.
pub fn recent_views(points: &[RecentPoint]) -> Vec<RecentPointView> {
    points
        .iter()
        .filter_map(|point| {
            Some(RecentPointView {
                time: point.time?.0 as i64,
                timespan: point.timespan.map(|n| n.0).unwrap_or_default(),
                count_in: point.count_in.map(|n| n.0).unwrap_or_default(),
                count_out: point.count_out.map(|n| n.0).unwrap_or_default(),
                spam_in: point.spam_in.map(|n| n.0).unwrap_or_default(),
                virus_in: point.virus_in.map(|n| n.0).unwrap_or_default(),
            })
        })
        .collect()
}

/// Débit instantané, tiré de la dernière tranche complète de la courbe.
///
/// La dernière tranche renvoyée est en cours et toujours sous-remplie : on prend
/// l'avant-dernière, seule à couvrir sa durée entière. `None` quand il n'y a pas
/// assez de points, ou quand la tranche n'a pas de durée.
pub fn throughput_samples(points: &[RecentPointView], ts_ms: i64) -> Vec<Sample> {
    let Some(point) = points.iter().rev().nth(1) else {
        return Vec::new();
    };
    if point.timespan <= 0.0 {
        return Vec::new();
    }
    let per_minute = |count: f64| count / point.timespan * 60.0;
    vec![
        gauge("mail_rate_in_per_minute", per_minute(point.count_in), ts_ms),
        gauge("mail_rate_out_per_minute", per_minute(point.count_out), ts_ms),
    ]
}

/// `GET /statistics/spamscores` : la répartition par niveau de spam.
pub fn spam_score_views(scores: &[SpamScore]) -> Vec<SpamScoreView> {
    scores
        .iter()
        .filter_map(|score| {
            Some(SpamScoreView {
                level: score.level.clone()?,
                count: score.count.map(|n| n.0).unwrap_or_default(),
                ratio_percent: score.ratio.map(|n| n.0 * 100.0),
            })
        })
        .collect()
}

pub fn spam_score_samples(views: &[SpamScoreView], ts_ms: i64) -> Vec<Sample> {
    views
        .iter()
        .map(|view| {
            gauge("spam_score_messages", view.count, ts_ms).with_label("level", view.level.clone())
        })
        .collect()
}

/// `GET /statistics/virus` : les virus les plus vus, plafonnés.
pub fn virus_views(stats: &[VirusStat]) -> Vec<VirusView> {
    let mut views: Vec<VirusView> = stats
        .iter()
        .filter_map(|stat| {
            Some(VirusView {
                name: stat.name.clone()?,
                count: stat.count.map(|n| n.0).unwrap_or_default(),
            })
        })
        .collect();
    views.sort_by(|a, b| {
        b.count
            .partial_cmp(&a.count)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    views.truncate(MAX_VIRUSES);
    views
}

pub fn virus_samples(views: &[VirusView], ts_ms: i64) -> Vec<Sample> {
    views
        .iter()
        .map(|view| {
            gauge("virus_detections", view.count, ts_ms).with_label("virus", view.name.clone())
        })
        .collect()
}

// --------------------------------------------------------------------------
// Quarantaines
// --------------------------------------------------------------------------

/// Occupation des quarantaines. Le niveau de spam moyen ne sort que de la
/// quarantaine de spam ; la quarantaine antivirus ne le renvoie pas.
pub fn quarantine_view(
    spam: Option<&QuarantineStatus>,
    virus: Option<&QuarantineStatus>,
    attachment_count: Option<f64>,
) -> QuarantineView {
    // PMG donne une occupation en mébioctets : on la ramène en octets, unité de
    // toutes les tailles de DumbMonit.
    let bytes = |status: Option<&QuarantineStatus>| {
        status.and_then(|s| s.mbytes).map(|n| n.0 * 1_048_576.0)
    };
    QuarantineView {
        spam_count: spam.and_then(|s| s.count).map(|n| n.0),
        spam_bytes: bytes(spam),
        spam_avg_level: spam.and_then(|s| s.avgspam).map(|n| n.0),
        virus_count: virus.and_then(|s| s.count).map(|n| n.0),
        virus_bytes: bytes(virus),
        attachment_count,
    }
}

pub fn quarantine_samples(view: &QuarantineView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |kind: &str, metric: &str, value: Option<f64>| {
        if let Some(value) = value {
            samples.push(gauge(metric, value, ts_ms).with_label("kind", kind.to_string()));
        }
    };

    push("spam", "quarantine_messages", view.spam_count);
    push("spam", "quarantine_bytes", view.spam_bytes);
    push("virus", "quarantine_messages", view.virus_count);
    push("virus", "quarantine_bytes", view.virus_bytes);
    push("attachment", "quarantine_messages", view.attachment_count);

    if let Some(level) = view.spam_avg_level {
        samples.push(gauge("quarantine_avg_spam_level", level, ts_ms));
    }
    samples
}

// --------------------------------------------------------------------------
// Services
// --------------------------------------------------------------------------

/// Les unités déclarées par la passerelle, celles qui ne sont pas installées en
/// moins : un `chrony` absent parce que la machine utilise `systemd-timesyncd`
/// n'est pas un service en panne.
pub fn service_views(entries: &[ServiceEntry]) -> Vec<ServiceView> {
    entries
        .iter()
        .filter(|entry| !entry.absent())
        .filter_map(|entry| {
            Some(ServiceView {
                service: entry.id()?.to_string(),
                description: entry.desc.clone().filter(|d| !d.is_empty()),
                state: entry.state.clone(),
                unit_state: entry.unit_state.clone(),
                running: entry.running(),
            })
        })
        .collect()
}

pub fn service_samples(node: &str, views: &[ServiceView], ts_ms: i64) -> Vec<Sample> {
    views
        .iter()
        .map(|view| {
            gauge("service_running", if view.running { 1.0 } else { 0.0 }, ts_ms)
                .with_label("node", node.to_string())
                .with_label("service", view.service.clone())
        })
        .collect()
}

// --------------------------------------------------------------------------
// Bases de signatures
// --------------------------------------------------------------------------

/// Lit l'horodatage d'une base ClamAV.
///
/// ClamAV écrit sa date dans l'en-tête d'un `.cvd` sous une forme à lui :
/// `16 Dec 2025 23-18 +0000` — un tiret là où tout le monde met deux points. PMG
/// la recopie telle quelle. `None` si le format change : mieux vaut pas d'âge du
/// tout qu'un âge de cinquante ans.
pub fn parse_clamav_time(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    // `%H-%M` et non `%H:%M` : c'est la particularité du format.
    chrono::DateTime::parse_from_str(raw, "%d %b %Y %H-%M %z")
        .ok()
        .map(|date| date.timestamp())
        .or_else(|| {
            chrono::DateTime::parse_from_str(raw, "%d %b %Y %H:%M %z")
                .ok()
                .map(|date| date.timestamp())
        })
}

pub fn clamav_views(databases: &[ClamavDatabase]) -> Vec<SignatureView> {
    databases
        .iter()
        .filter_map(|db| {
            Some(SignatureView {
                name: db.name.clone().or_else(|| db.kind.clone())?,
                version: db.version.clone(),
                updated_at: db.build_time.as_deref().and_then(parse_clamav_time),
                signatures: db.nsigs.map(|n| n.0),
                update_available: None,
            })
        })
        .collect()
}

pub fn spamassassin_views(channels: &[SpamassassinChannel]) -> Vec<SignatureView> {
    channels
        .iter()
        .filter_map(|channel| {
            Some(SignatureView {
                name: channel.channel.clone()?,
                version: channel.version.clone(),
                updated_at: channel.last_updated.map(|n| n.0 as i64),
                signatures: None,
                update_available: channel.update_avail.map(|n| n.0 != 0.0),
            })
        })
        .collect()
}

/// Séries d'une famille de bases de signatures.
///
/// `family` vaut `virus` (ClamAV) ou `spam` (SpamAssassin). L'âge n'est produit
/// que si la base porte une date : un canal jamais mis à jour n'a pas de
/// `last_updated`, et prétendre qu'il date d'aujourd'hui masquerait exactement la
/// panne que l'on cherche.
pub fn signature_samples(
    node: &str,
    family: &str,
    views: &[SignatureView],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    for view in views {
        let label = |sample: Sample| {
            sample
                .with_label("node", node.to_string())
                .with_label("family", family.to_string())
                .with_label("database", view.name.clone())
        };
        if let Some(updated) = view.updated_at {
            let age = (now_s - updated).max(0) as f64;
            samples.push(label(gauge("signature_age_seconds", age, ts_ms)));
        }
        if let Some(count) = view.signatures {
            samples.push(label(gauge("signature_count", count, ts_ms)));
        }
        if let Some(available) = view.update_available {
            samples.push(label(gauge(
                "signature_update_available",
                if available { 1.0 } else { 0.0 },
                ts_ms,
            )));
        }
    }
    samples
}

// --------------------------------------------------------------------------
// Certificats, abonnement, mises à jour, grappe
// --------------------------------------------------------------------------

/// Les certificats servis par l'interface d'administration.
pub fn certificate_views(infos: &[CertificateInfo]) -> Vec<CertificateView> {
    infos
        .iter()
        .filter_map(|info| {
            Some(CertificateView {
                filename: info.filename.clone()?,
                subject: info.subject.clone(),
                issuer: info.issuer.clone(),
                not_after: info.notafter.map(|n| n.0 as i64),
                san: info.san.clone(),
            })
        })
        .collect()
}

pub fn certificate_samples(
    node: &str,
    views: &[CertificateView],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    views
        .iter()
        .filter_map(|view| {
            let not_after = view.not_after?;
            Some(
                gauge("certificate_expires_in_seconds", (not_after - now_s) as f64, ts_ms)
                    .with_label("node", node.to_string())
                    .with_label("certificate", view.filename.clone()),
            )
        })
        .collect()
}

/// L'abonnement du nœud. `notfound` est la réponse d'une installation sans clé :
/// c'est l'état normal d'un homelab, pas une anomalie.
pub fn subscription_view(subscription: &Subscription) -> Option<SubscriptionView> {
    Some(SubscriptionView {
        status: subscription.status.clone()?,
        level: subscription.level.clone().filter(|level| !level.is_empty()),
        next_due_date: subscription.nextduedate.clone().filter(|date| !date.is_empty()),
    })
}

pub fn subscription_samples(node: &str, view: &SubscriptionView, ts_ms: i64) -> Vec<Sample> {
    let active = view.status.eq_ignore_ascii_case("active");
    vec![
        gauge("subscription_active", if active { 1.0 } else { 0.0 }, ts_ms)
            .with_label("node", node.to_string())
            .with_label("status", view.status.clone())
            .with_label("level", view.level.clone().unwrap_or_default()),
    ]
}

/// Mises à jour en attente, dont celles issues d'un dépôt de sécurité.
pub fn updates_samples(node: &str, updates: &[AptUpdate], ts_ms: i64) -> Vec<Sample> {
    let security = updates.iter().filter(|u| u.is_security()).count();
    vec![
        gauge("node_updates_pending", updates.len() as f64, ts_ms),
        gauge("node_updates_security_pending", security as f64, ts_ms),
    ]
    .into_iter()
    .map(|sample| sample.with_label("node", node.to_string()))
    .collect()
}

/// Les nœuds de la grappe. Une installation autonome répond une liste vide :
/// il n'y a alors ni série ni ligne à afficher, et c'est normal.
pub fn cluster_views(nodes: &[ClusterNode]) -> Vec<ClusterNodeView> {
    nodes
        .iter()
        .filter_map(|node| {
            Some(ClusterNodeView {
                name: node.name.clone()?,
                ip: node.ip.clone(),
                role: node.kind.clone(),
                insync: node.insync.map(|n| n.0 != 0.0),
                error: node.conn_error.clone().filter(|error| !error.is_empty()),
            })
        })
        .collect()
}

pub fn cluster_samples(views: &[ClusterNodeView], ts_ms: i64) -> Vec<Sample> {
    if views.is_empty() {
        return Vec::new();
    }
    let mut samples = vec![gauge("cluster_nodes", views.len() as f64, ts_ms)];
    for view in views {
        let label = |sample: Sample| {
            sample
                .with_label("node", view.name.clone())
                .with_label("role", view.role.clone().unwrap_or_default())
        };
        if let Some(insync) = view.insync {
            samples.push(label(gauge(
                "cluster_node_insync",
                if insync { 1.0 } else { 0.0 },
                ts_ms,
            )));
        }
        samples.push(label(gauge(
            "cluster_node_healthy",
            if view.error.is_none() { 1.0 } else { 0.0 },
            ts_ms,
        )));
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valeur(samples: &[Sample], metric: &str) -> Option<f64> {
        samples.iter().find(|s| s.metric == format!("{P}{metric}")).map(|s| s.value)
    }

    fn valeur_etiquetee(samples: &[Sample], metric: &str, key: &str, value: &str) -> Option<f64> {
        samples
            .iter()
            .find(|s| {
                s.metric == format!("{P}{metric}")
                    && s.labels.get(key).map(String::as_str) == Some(value)
            })
            .map(|s| s.value)
    }

    /// Relevé réel d'une passerelle 9.1.
    const STATUS: &str = r#"{
      "cpu": 0.12, "wait": 0.03, "insync": 1, "uptime": 716766, "time": 1790078488,
      "loadavg": ["0.82", "1.94", "1.70"],
      "memory": {"free": 3148816384, "total": 16000000000, "used": 4000000000},
      "swap": {"used": 0, "total": 8589930496},
      "rootfs": {"avail": 60000000000, "used": 40000000000, "total": 100000000000},
      "kversion": "Linux 6.14.8-2-pve",
      "cpuinfo": {"cpus": 8, "cores": 6, "sockets": 1}
    }"#;

    #[test]
    fn letat_du_noeud_donne_ses_series_avec_letiquette_du_noeud() {
        let status: NodeStatus = serde_json::from_str(STATUS).unwrap();
        let samples = node_samples("mail1", &status, 1_790_078_488, 0);

        assert_eq!(valeur(&samples, "node_cpu_percent"), Some(12.0));
        assert_eq!(valeur(&samples, "node_iowait_percent"), Some(3.0));
        assert_eq!(valeur(&samples, "node_cpu_count"), Some(8.0));
        assert_eq!(valeur(&samples, "node_load1"), Some(0.82));
        assert_eq!(valeur(&samples, "node_memory_used_percent"), Some(25.0));
        assert_eq!(valeur(&samples, "node_rootfs_percent"), Some(40.0));
        assert_eq!(valeur(&samples, "node_uptime_seconds"), Some(716_766.0));
        assert_eq!(valeur(&samples, "node_insync"), Some(1.0));
        assert_eq!(valeur(&samples, "node_clock_offset_seconds"), Some(0.0));
        assert!(
            samples.iter().all(|s| s.labels.get("node").map(String::as_str) == Some("mail1")),
            "toute série de nœud porte son étiquette"
        );
    }

    #[test]
    fn un_etat_vide_ne_produit_aucune_serie_inventee() {
        let status = NodeStatus::default();
        let samples = node_samples("mail1", &status, 1_790_078_488, 0);
        assert!(samples.is_empty(), "aucun zéro ne doit passer pour une mesure : {samples:?}");
    }

    #[test]
    fn la_version_du_noeud_se_lit_dans_la_chaine_de_paquet() {
        let status = NodeStatus {
            pmgversion: Some("pmg-api/9.1.2/42245585286a".into()),
            ..Default::default()
        };
        assert_eq!(node_version(&status), Some("9.1.2".to_string()));
        let samples = node_samples("mail1", &status, 0, 0);
        assert_eq!(
            samples
                .iter()
                .find(|s| s.metric == format!("{P}node_version_info"))
                .and_then(|s| s.labels.get("version"))
                .map(String::as_str),
            Some("9.1.2")
        );
        assert!(node_version(&NodeStatus::default()).is_none());
        let etrange = NodeStatus { pmgversion: Some("pmg-api".into()), ..Default::default() };
        assert!(node_version(&etrange).is_none(), "un format inattendu ne donne pas de version");
    }

    #[test]
    fn une_horloge_absente_ou_aberrante_ne_donne_pas_decart() {
        assert!(clock_offset(&NodeStatus::default(), 1_790_000_000).is_none());
        let status = NodeStatus { time: Some(Num(0.0)), ..Default::default() };
        assert!(clock_offset(&status, 1_790_000_000).is_none());
        let status = NodeStatus { time: Some(Num(1_790_000_042.0)), ..Default::default() };
        assert_eq!(clock_offset(&status, 1_790_000_000), Some(42.0));
    }

    /// Relevé réel : sept messages différés, cinq récents et deux de plus de
    /// quarante minutes. Toutes les colonnes sont des chaînes.
    const QSHAPE: &str = r#"[
      {"domain":"TOTAL","total":"7","5m":"5","10m":"0","20m":"0","40m":"2",
       "80m":"0","160m":"0","320m":"0","640m":"0","1280m":"0","1280m+":"0"},
      {"domain":"example.net","total":"5","5m":"5"},
      {"domain":"home.arpa","total":"2","40m":"2"}
    ]"#;

    #[test]
    fn une_file_dattente_se_lit_dans_la_ligne_totale() {
        let rows: Vec<QshapeRow> = serde_json::from_str(QSHAPE).unwrap();
        let view = queue_view("deferred", &rows);

        assert_eq!(view.messages, 7.0);
        assert_eq!(view.domains, 2.0);
        assert_eq!(
            view.oldest_age_seconds,
            Some(1_200.0),
            "la tranche « 40m » commence à vingt minutes"
        );
        assert_eq!(view.top_domains[0].domain, "example.net");
        assert_eq!(view.top_domains[0].messages, 5.0);

        let samples = queue_samples(&view, 0);
        assert_eq!(valeur(&samples, "queue_messages"), Some(7.0));
        assert_eq!(valeur(&samples, "queue_oldest_age_seconds"), Some(1_200.0));
        assert!(
            samples.iter().all(|s| s.labels.get("queue").map(String::as_str) == Some("deferred"))
        );
    }

    #[test]
    fn une_file_vide_ne_donne_aucun_age() {
        let rows: Vec<QshapeRow> =
            serde_json::from_str(r#"[{"domain":"TOTAL","total":"0","5m":"0","1280m+":"0"}]"#)
                .unwrap();
        let view = queue_view("active", &rows);
        assert_eq!(view.messages, 0.0);
        assert_eq!(view.domains, 0.0);
        assert!(view.oldest_age_seconds.is_none(), "une file vide n'a pas de plus vieux message");
        let samples = queue_samples(&view, 0);
        assert!(valeur(&samples, "queue_oldest_age_seconds").is_none());
        assert_eq!(valeur(&samples, "queue_messages"), Some(0.0), "zéro message est une mesure");
    }

    #[test]
    fn sans_ligne_totale_les_domaines_sont_sommes() {
        let rows: Vec<QshapeRow> = serde_json::from_str(
            r#"[{"domain":"a.net","total":"3"},{"domain":"b.net","total":"4"}]"#,
        )
        .unwrap();
        let view = queue_view("hold", &rows);
        assert_eq!(view.messages, 7.0);
        assert_eq!(view.domains, 2.0);
    }

    /// Relevé réel des totaux du jour.
    const MAIL: &str = r#"{
      "count": 40, "count_in": 40, "count_out": 0, "bytes_in": 22109, "bytes_out": 0,
      "spamcount_in": 8, "spamcount_out": 0, "viruscount_in": 2, "viruscount_out": 0,
      "bounces_in": 2, "bounces_out": 0, "junk_in": 10, "junk_out": 0,
      "glcount": 0, "spfcount": 0, "rbl_rejects": 0, "pregreet_rejects": 0,
      "avptime": 0.684675025939941
    }"#;

    #[test]
    fn les_totaux_du_jour_donnent_leurs_series_et_la_part_dindesirables() {
        let stats: MailStats = serde_json::from_str(MAIL).unwrap();
        let view = mail_view(&stats);
        let samples = mail_samples(&view, 0);

        assert_eq!(valeur(&samples, "mail_count_in"), Some(40.0));
        assert_eq!(valeur(&samples, "mail_spam_in"), Some(8.0));
        assert_eq!(valeur(&samples, "mail_virus_in"), Some(2.0));
        assert_eq!(valeur(&samples, "mail_bounces_in"), Some(2.0));
        assert_eq!(valeur(&samples, "mail_junk_percent"), Some(25.0));
        assert_eq!(valeur(&samples, "mail_junk_out"), Some(0.0));
        assert_eq!(valeur(&samples, "mail_avg_processing_seconds"), Some(0.684675025939941));
    }

    #[test]
    fn une_journee_sans_courrier_na_pas_de_part_dindesirables() {
        let stats: MailStats = serde_json::from_str(r#"{"count_in":0,"junk_in":0}"#).unwrap();
        let samples = mail_samples(&mail_view(&stats), 0);
        assert_eq!(valeur(&samples, "mail_count_in"), Some(0.0));
        assert!(
            valeur(&samples, "mail_junk_percent").is_none(),
            "0 % sur une journée vide serait une bonne nouvelle imaginaire"
        );
    }

    #[test]
    fn le_debit_se_lit_sur_lavant_derniere_tranche_complete() {
        let points: Vec<RecentPoint> = serde_json::from_str(
            r#"[{"time":1000,"timespan":1800,"count_in":30,"count_out":6},
                {"time":2800,"timespan":1800,"count_in":90,"count_out":30},
                {"time":4600,"timespan":1800,"count_in":3,"count_out":0}]"#,
        )
        .unwrap();
        let views = recent_views(&points);
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].time, 1000);

        let samples = throughput_samples(&views, 0);
        assert_eq!(valeur(&samples, "mail_rate_in_per_minute"), Some(3.0));
        assert_eq!(valeur(&samples, "mail_rate_out_per_minute"), Some(1.0));

        assert!(throughput_samples(&views[..1], 0).is_empty(), "un seul point ne dit rien");
        assert!(throughput_samples(&[], 0).is_empty());
    }

    #[test]
    fn la_quarantaine_se_compte_en_octets_et_jamais_en_messages_lus() {
        let spam: QuarantineStatus = serde_json::from_str(
            r#"{"count":8,"mbytes":0.03125,"avgbytes":"534.0","avgspam":"7.5"}"#,
        )
        .unwrap();
        let virus: QuarantineStatus =
            serde_json::from_str(r#"{"count":2,"mbytes":0.5,"avgbytes":0}"#).unwrap();
        let view = quarantine_view(Some(&spam), Some(&virus), Some(3.0));

        assert_eq!(view.spam_count, Some(8.0));
        assert_eq!(view.spam_bytes, Some(32_768.0));
        assert_eq!(view.spam_avg_level, Some(7.5));
        assert_eq!(view.attachment_count, Some(3.0));

        let samples = quarantine_samples(&view, 0);
        assert_eq!(valeur_etiquetee(&samples, "quarantine_messages", "kind", "spam"), Some(8.0));
        assert_eq!(valeur_etiquetee(&samples, "quarantine_messages", "kind", "virus"), Some(2.0));
        assert_eq!(
            valeur_etiquetee(&samples, "quarantine_messages", "kind", "attachment"),
            Some(3.0)
        );
        assert_eq!(valeur(&samples, "quarantine_avg_spam_level"), Some(7.5));
    }

    #[test]
    fn une_quarantaine_non_interrogee_ne_produit_rien() {
        let view = quarantine_view(None, None, None);
        assert!(quarantine_samples(&view, 0).is_empty());
    }

    #[test]
    fn un_service_non_installe_nest_pas_un_service_en_panne() {
        let entries: Vec<ServiceEntry> = serde_json::from_str(
            r#"[{"service":"postfix","desc":"Postfix","state":"running","active-state":"active","unit-state":"enabled"},
                {"service":"pmg-smtp-filter","desc":"Filter","state":"dead","active-state":"inactive","unit-state":"enabled"},
                {"service":"chrony","desc":"","state":"unknown","active-state":"unknown","unit-state":"not-found"}]"#,
        )
        .unwrap();
        let views = service_views(&entries);
        assert_eq!(views.len(), 2, "chrony n'est pas installé : aucune ligne");

        let samples = service_samples("mail1", &views, 0);
        assert_eq!(valeur_etiquetee(&samples, "service_running", "service", "postfix"), Some(1.0));
        assert_eq!(
            valeur_etiquetee(&samples, "service_running", "service", "pmg-smtp-filter"),
            Some(0.0)
        );
        assert!(valeur_etiquetee(&samples, "service_running", "service", "chrony").is_none());
    }

    #[test]
    fn la_date_dune_base_clamav_se_lit_dans_son_format_a_elle() {
        // `23-18` et non `23:18` : c'est ainsi que ClamAV l'écrit.
        assert_eq!(parse_clamav_time("16 Dec 2025 23-18 +0000"), Some(1_765_927_080));
        assert_eq!(parse_clamav_time(" 11 Sep 2025 08-29 -0400 "), Some(1_757_593_740));
        // Tolérance pour une graphie plus courante, si elle apparaissait.
        assert_eq!(parse_clamav_time("16 Dec 2025 23:18 +0000"), Some(1_765_927_080));
        assert!(parse_clamav_time("hier").is_none());
        assert!(parse_clamav_time("").is_none());
    }

    #[test]
    fn lage_des_signatures_ne_sinvente_pas_quand_la_date_manque() {
        let bases: Vec<ClamavDatabase> = serde_json::from_str(
            r#"[{"name":"daily","type":"ClamAV-VDB","build_time":"22 Sep 2026 06-27 +0000",
                 "nsigs":"355666","version":"28131"},
                {"name":"main","type":"ClamAV-VDB","build_time":"date illisible","nsigs":"3287027"}]"#,
        )
        .unwrap();
        let views = clamav_views(&bases);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].signatures, Some(355_666.0));
        assert!(views[1].updated_at.is_none());

        // 22 septembre 2026 à 12 h UTC, soit cinq heures et demie après la base.
        let samples = signature_samples("mail1", "virus", &views, 1_790_078_400, 0);
        let age = valeur_etiquetee(&samples, "signature_age_seconds", "database", "daily");
        assert_eq!(age, Some(19_980.0));
        assert!(
            valeur_etiquetee(&samples, "signature_age_seconds", "database", "main").is_none(),
            "une date illisible ne doit pas produire un âge inventé"
        );
        assert_eq!(
            valeur_etiquetee(&samples, "signature_count", "database", "main"),
            Some(3_287_027.0)
        );
    }

    #[test]
    fn un_canal_spamassassin_jamais_mis_a_jour_na_pas_dage() {
        let channels: Vec<SpamassassinChannel> = serde_json::from_str(
            r#"[{"channel":"updates.spamassassin.org","update_avail":0,"version":"1938404",
                 "last_updated":1790078571},
                {"channel":"kam.sa-channels.mcgrail.com","update_avail":1}]"#,
        )
        .unwrap();
        let views = spamassassin_views(&channels);
        assert_eq!(views[0].update_available, Some(false));
        assert!(views[1].updated_at.is_none());

        let samples = signature_samples("mail1", "spam", &views, 1_790_078_571, 0);
        assert_eq!(
            valeur_etiquetee(
                &samples,
                "signature_age_seconds",
                "database",
                "updates.spamassassin.org"
            ),
            Some(0.0)
        );
        assert_eq!(
            valeur_etiquetee(
                &samples,
                "signature_update_available",
                "database",
                "kam.sa-channels.mcgrail.com"
            ),
            Some(1.0)
        );
    }

    #[test]
    fn un_certificat_donne_le_temps_qui_lui_reste() {
        let infos: Vec<CertificateInfo> = serde_json::from_str(
            r#"[{"filename":"pmg-api.pem","subject":"/CN=mail1","issuer":"/CN=mail1",
                 "notafter":1790164800,"notbefore":1790078400,"san":[]},
                {"filename":"pmg-tls.pem"}]"#,
        )
        .unwrap();
        let views = certificate_views(&infos);
        assert_eq!(views.len(), 2);

        let samples = certificate_samples("mail1", &views, 1_790_078_400, 0);
        assert_eq!(samples.len(), 1, "un certificat sans date d'expiration ne produit rien");
        assert_eq!(
            valeur_etiquetee(
                &samples,
                "certificate_expires_in_seconds",
                "certificate",
                "pmg-api.pem"
            ),
            Some(86_400.0)
        );
    }

    #[test]
    fn un_abonnement_absent_est_un_etat_normal_et_pas_une_erreur() {
        let subscription: Subscription = serde_json::from_str(
            r#"{"status":"notfound","message":"There is no subscription key"}"#,
        )
        .unwrap();
        let view = subscription_view(&subscription).unwrap();
        let samples = subscription_samples("mail1", &view, 0);
        assert_eq!(valeur(&samples, "subscription_active"), Some(0.0));
        assert_eq!(
            samples[0].labels.get("status").map(String::as_str),
            Some("notfound"),
            "l'état exact reste lisible en étiquette"
        );

        let subscription: Subscription =
            serde_json::from_str(r#"{"status":"active","level":"c"}"#).unwrap();
        let samples = subscription_samples("mail1", &subscription_view(&subscription).unwrap(), 0);
        assert_eq!(valeur(&samples, "subscription_active"), Some(1.0));
    }

    #[test]
    fn les_mises_a_jour_distinguent_celles_de_securite() {
        let updates: Vec<AptUpdate> = serde_json::from_str(
            r#"[{"Package":"libc6","Origin":"Debian-Security"},
                {"Package":"pmg-api","Origin":"Proxmox"},
                {"Package":"openssl","Origin":"Debian-Security"}]"#,
        )
        .unwrap();
        let samples = updates_samples("mail1", &updates, 0);
        assert_eq!(valeur(&samples, "node_updates_pending"), Some(3.0));
        assert_eq!(valeur(&samples, "node_updates_security_pending"), Some(2.0));

        let aucune = updates_samples("mail1", &[], 0);
        assert_eq!(valeur(&aucune, "node_updates_pending"), Some(0.0), "zéro est une mesure");
    }

    #[test]
    fn une_installation_autonome_na_pas_de_grappe() {
        assert!(cluster_views(&[]).is_empty());
        assert!(cluster_samples(&[], 0).is_empty(), "pas de grappe n'est pas une grappe dégradée");
    }

    #[test]
    fn un_noeud_de_grappe_desynchronise_se_voit() {
        let nodes: Vec<ClusterNode> = serde_json::from_str(
            r#"[{"cid":1,"name":"mail1","ip":"10.0.0.41","type":"master","insync":1},
                {"cid":2,"name":"mail2","ip":"10.0.0.42","type":"node","insync":0,
                 "conn_error":"connection refused"}]"#,
        )
        .unwrap();
        let views = cluster_views(&nodes);
        assert_eq!(views.len(), 2);
        assert_eq!(views[1].error.as_deref(), Some("connection refused"));

        let samples = cluster_samples(&views, 0);
        assert_eq!(valeur(&samples, "cluster_nodes"), Some(2.0));
        assert_eq!(valeur_etiquetee(&samples, "cluster_node_insync", "node", "mail1"), Some(1.0));
        assert_eq!(valeur_etiquetee(&samples, "cluster_node_insync", "node", "mail2"), Some(0.0));
        assert_eq!(valeur_etiquetee(&samples, "cluster_node_healthy", "node", "mail2"), Some(0.0));
    }

    #[test]
    fn les_virus_les_plus_vus_sont_plafonnes_et_ordonnes() {
        let stats: Vec<VirusStat> = (0..15)
            .map(|i| VirusStat { name: Some(format!("Virus-{i}")), count: Some(Num(i as f64)) })
            .collect();
        let views = virus_views(&stats);
        assert_eq!(views.len(), MAX_VIRUSES);
        assert_eq!(views[0].name, "Virus-14", "le plus vu passe devant");

        let samples = virus_samples(&views, 0);
        assert_eq!(valeur_etiquetee(&samples, "virus_detections", "virus", "Virus-14"), Some(14.0));
    }

    #[test]
    fn la_repartition_par_niveau_de_spam_garde_ses_niveaux() {
        let scores: Vec<SpamScore> = serde_json::from_str(
            r#"[{"level":"0","count":40,"ratio":0.8},{"level":"10","count":10,"ratio":0.2}]"#,
        )
        .unwrap();
        let views = spam_score_views(&scores);
        assert_eq!(views[1].ratio_percent, Some(20.0));
        let samples = spam_score_samples(&views, 0);
        assert_eq!(valeur_etiquetee(&samples, "spam_score_messages", "level", "10"), Some(10.0));
    }
}
