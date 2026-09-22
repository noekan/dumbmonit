//! Conversion des réponses de l'API en échantillons et en vue.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//!
//! Deux principes gouvernent l'ensemble :
//!
//! * **Ce que l'API ne dit pas ne devient jamais zéro.** Un champ absent — vieille
//!   version, droit manquant, remote injoignable — donne une métrique en moins,
//!   jamais un 0 qui se ferait passer pour une mesure.
//! * **Les étiquettes sont stables.** `remote` nomme l'instance fédérée,
//!   `type` son produit (`pve` ou `pbs`) ; rien d'autre ne varie d'une
//!   interrogation à l'autre.

use std::collections::BTreeMap;

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    AptUpdate, CertificateInfo, MetricCollection, NodeStatus, Num, RemoteEntry, RemoteResources,
    RemoteSubscription, ResourcesStatus, Subscription, TaskEntry, Version,
};
use super::view::{
    CertificateView, EstateView, NodeView, ProbeView, RemoteView, SubscriptionView, TaskView,
};

/// Préfixe commun à toutes les métriques de l'intégration.
///
/// Il isole l'intégration, comme `pbs_` pour le serveur de sauvegarde : sans lui,
/// `node_cpu_percent` entrerait en collision avec la même notion venue de
/// l'hyperviseur ou du serveur de sauvegarde.
pub const P: &str = "pdm_";

pub fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Pourcentage d'occupation, ou `None` si le total est inconnu ou nul — mieux
/// vaut pas de point du tout qu'un 0 % trompeur.
fn percent(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    let total = total?;
    let used = used?;
    (total > 0.0).then(|| used / total * 100.0)
}

fn num(value: Option<Num>) -> Option<f64> {
    value.map(|n| n.0)
}

/// Somme de deux mesures dont chacune peut manquer : `None` seulement si les
/// deux manquent.
fn add(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

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

// --------------------------------------------------------------------------
// Le parc entier
// --------------------------------------------------------------------------

/// `GET /resources/status` : ce que la console additionne pour tout le parc.
///
/// C'est la raison d'être d'un équipement PDM : un seul appel donne les invités
/// démarrés et arrêtés, les nœuds en ligne, le processeur, la mémoire et le
/// stockage de l'ensemble des clusters.
pub fn estate_view(status: &ResourcesStatus) -> EstateView {
    EstateView {
        remotes: num(status.remotes),
        remotes_failed: num(status.failed_remotes),
        nodes_online: add(num(status.pve_nodes.online), num(status.pbs_nodes.online)),
        nodes_offline: add(num(status.pve_nodes.offline), num(status.pbs_nodes.offline)),
        qemu_running: num(status.qemu.running),
        qemu_stopped: num(status.qemu.stopped),
        lxc_running: num(status.lxc.running),
        lxc_stopped: num(status.lxc.stopped),
        cpu_used_cores: add(num(status.pve_cpu_stats.used), num(status.pbs_cpu_stats.used)),
        cpu_total_cores: add(num(status.pve_cpu_stats.max), num(status.pbs_cpu_stats.max)),
        memory_used_bytes: add(
            num(status.pve_memory_stats.used),
            num(status.pbs_memory_stats.used),
        ),
        memory_total_bytes: add(
            num(status.pve_memory_stats.total),
            num(status.pbs_memory_stats.total),
        ),
        storage_used_bytes: add(
            num(status.pve_storage_stats.used),
            num(status.pbs_storage_stats.used),
        ),
        storage_total_bytes: add(
            num(status.pve_storage_stats.total),
            num(status.pbs_storage_stats.total),
        ),
        datastores: num(status.pbs_datastores.online),
    }
}

pub fn estate_samples(estate: &EstateView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |metric: &str, value: Option<f64>| {
        if let Some(value) = value {
            samples.push(gauge(metric, value, ts_ms));
        }
    };

    push("remotes_total", estate.remotes);
    push("remotes_failed", estate.remotes_failed);
    push("nodes_online", estate.nodes_online);
    push("nodes_offline", estate.nodes_offline);
    push("cpu_used_cores", estate.cpu_used_cores);
    push("cpu_total_cores", estate.cpu_total_cores);
    push("memory_used_bytes", estate.memory_used_bytes);
    push("memory_total_bytes", estate.memory_total_bytes);
    push("memory_used_percent", percent(estate.memory_used_bytes, estate.memory_total_bytes));
    push("storage_used_bytes", estate.storage_used_bytes);
    push("storage_total_bytes", estate.storage_total_bytes);
    push("storage_used_percent", percent(estate.storage_used_bytes, estate.storage_total_bytes));
    push("datastores_total", estate.datastores);

    // Les invités portent le type en étiquette : une seule métrique par état,
    // `guest_type="qemu"` ou `"lxc"`.
    for (kind, running, stopped) in [
        ("qemu", estate.qemu_running, estate.qemu_stopped),
        ("lxc", estate.lxc_running, estate.lxc_stopped),
    ] {
        if let Some(value) = running {
            samples.push(gauge("guests_running", value, ts_ms).with_label("guest_type", kind));
        }
        if let Some(value) = stopped {
            samples.push(gauge("guests_stopped", value, ts_ms).with_label("guest_type", kind));
        }
    }

    samples
}

// --------------------------------------------------------------------------
// Les instances fédérées
// --------------------------------------------------------------------------

/// Construit une vue par remote, à partir de tout ce qui a pu être lu.
///
/// Chaque source est facultative : sans `/resources/list`, on connaît encore
/// l'existence et la joignabilité des instances ; sans `/resources/status`, il
/// reste la liste configurée. Une instance déclarée mais jamais vue vaut mieux
/// qu'une absence de ligne — c'est précisément celle-là qu'il faut montrer.
pub fn remote_views(
    configured: &[RemoteEntry],
    status: Option<&ResourcesStatus>,
    resources: &[RemoteResources],
    subscriptions: &[RemoteSubscription],
    collections: &[MetricCollection],
) -> Vec<RemoteView> {
    let mut views: BTreeMap<String, RemoteView> = BTreeMap::new();

    for entry in configured {
        views.insert(
            entry.id.clone(),
            RemoteView {
                id: entry.id.clone(),
                kind: entry.kind.clone(),
                // Optimiste par défaut : seul un état ou une erreur constatée
                // fait basculer une instance à « injoignable ».
                reachable: true,
                // L'empreinte de certificat suit l'adresse dans la configuration ;
                // elle n'apprend rien à personne et alourdit l'affichage.
                nodes: entry.nodes.iter().map(|node| strip_fingerprint(node)).collect(),
                ..Default::default()
            },
        );
    }

    if let Some(status) = status {
        for remote in &status.remote_list {
            let view = views.entry(remote.name.clone()).or_insert_with(|| RemoteView {
                id: remote.name.clone(),
                reachable: true,
                ..Default::default()
            });
            if view.kind.is_none() {
                view.kind = remote.kind.clone();
            }
            let failed = remote.status.as_deref().is_some_and(|s| !s.eq_ignore_ascii_case("ok"));
            if failed {
                view.reachable = false;
                view.error = remote.messages.first().cloned();
            }
        }
    }

    for entry in resources {
        let view = views.entry(entry.remote.clone()).or_insert_with(|| RemoteView {
            id: entry.remote.clone(),
            reachable: true,
            ..Default::default()
        });
        if let Some(error) = &entry.error {
            view.reachable = false;
            if view.error.is_none() {
                view.error = Some(error.clone());
            }
            continue;
        }
        summarize_resources(view, entry);
    }

    for subscription in subscriptions {
        if let Some(view) = views.get_mut(&subscription.remote) {
            view.subscription = subscription.state.clone();
        }
    }

    for collection in collections {
        if let Some(view) = views.get_mut(&collection.remote) {
            view.last_collection = collection.last_collection;
            if view.error.is_none() {
                view.error = collection.error.clone();
            }
        }
    }

    views.into_values().collect()
}

/// Retire l'empreinte de certificat d'une adresse de nœud configurée :
/// `10.0.0.1:8006,fingerprint=AB:CD:…` devient `10.0.0.1:8006`.
fn strip_fingerprint(node: &str) -> String {
    node.split(',').next().unwrap_or(node).trim().to_string()
}

/// Additionne les ressources d'une instance : nœuds, invités, processeur,
/// mémoire, stockage, datastores.
///
/// Un stockage partagé est vu par chaque nœud du cluster : on ne compte chaque
/// nom de stockage qu'une fois, sans quoi la capacité d'un cluster de cinq nœuds
/// serait quintuplée.
fn summarize_resources(view: &mut RemoteView, entry: &RemoteResources) {
    let mut nodes_online = 0.0;
    let mut nodes_offline = 0.0;
    let mut guests_running = 0.0;
    let mut guests_stopped = 0.0;
    let mut cpu_used = 0.0;
    let mut cpu_total = 0.0;
    let mut memory_used = 0.0;
    let mut memory_total = 0.0;
    let mut datastores = 0.0;
    let mut seen_node = false;
    let mut seen_guest = false;
    let mut seen_store = false;
    let mut storages: BTreeMap<String, (f64, f64)> = BTreeMap::new();

    for resource in &entry.resources {
        match resource.kind.as_deref().unwrap_or_default() {
            "pve-node" | "pbs-node" => {
                seen_node = true;
                let online = resource
                    .status
                    .as_deref()
                    // Un nœud PBS n'a pas de statut : la console ne le liste que
                    // s'il a répondu.
                    .is_none_or(|status| status.eq_ignore_ascii_case("online"));
                if online {
                    nodes_online += 1.0;
                } else {
                    nodes_offline += 1.0;
                }
                cpu_used +=
                    resource.cpu.map_or(0.0, |c| c.0) * resource.maxcpu.map_or(0.0, |c| c.0);
                cpu_total += resource.maxcpu.map_or(0.0, |c| c.0);
                memory_used += resource.mem.map_or(0.0, |m| m.0);
                memory_total += resource.maxmem.map_or(0.0, |m| m.0);
            }
            "pve-qemu" | "pve-lxc" => {
                seen_guest = true;
                // Un modèle n'est ni démarré ni arrêté : il ne compte nulle part.
                if resource.template.unwrap_or(false) {
                    continue;
                }
                if resource.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("running")) {
                    guests_running += 1.0;
                } else {
                    guests_stopped += 1.0;
                }
            }
            "pve-storage" => {
                seen_store = true;
                let name =
                    resource.storage.clone().or_else(|| resource.id.clone()).unwrap_or_default();
                storages.insert(
                    name,
                    (resource.disk.map_or(0.0, |d| d.0), resource.maxdisk.map_or(0.0, |d| d.0)),
                );
            }
            "pbs-datastore" => {
                seen_store = true;
                datastores += 1.0;
                let name =
                    resource.name.clone().or_else(|| resource.id.clone()).unwrap_or_default();
                storages.insert(
                    name,
                    (resource.disk.map_or(0.0, |d| d.0), resource.maxdisk.map_or(0.0, |d| d.0)),
                );
            }
            _ => {}
        }
    }

    if seen_node {
        view.nodes_online = Some(nodes_online);
        view.nodes_offline = Some(nodes_offline);
        view.cpu_used_cores = Some(cpu_used);
        view.cpu_total_cores = Some(cpu_total);
        view.memory_used_bytes = Some(memory_used);
        view.memory_total_bytes = Some(memory_total);
    }
    if seen_guest {
        view.guests_running = Some(guests_running);
        view.guests_stopped = Some(guests_stopped);
    }
    if seen_store {
        let used: f64 = storages.values().map(|(used, _)| used).sum();
        let total: f64 = storages.values().map(|(_, total)| total).sum();
        view.storage_used_bytes = Some(used);
        view.storage_total_bytes = Some(total);
        view.datastores = Some(datastores);
    }
}

/// Marque les instances en retard de version sur leurs semblables.
///
/// Une console qui fédère cinq clusters sert d'abord à voir celui qu'on a oublié
/// de mettre à jour. On ne connaît pas la dernière version publiée par Proxmox —
/// et aller la chercher sur Internet n'est pas le métier d'une sonde —, alors on
/// compare les instances entre elles, produit par produit : celle qui traîne
/// derrière une autre du même type est signalée.
pub fn mark_versions_behind(remotes: &mut [RemoteView]) {
    let mut newest: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for remote in remotes.iter() {
        let Some(version) = remote.version.as_deref().map(parse_version) else { continue };
        let kind = remote.kind.clone().unwrap_or_default();
        let entry = newest.entry(kind).or_insert_with(|| version.clone());
        if version > *entry {
            *entry = version;
        }
    }
    for remote in remotes.iter_mut() {
        let Some(version) = remote.version.as_deref().map(parse_version) else { continue };
        let kind = remote.kind.clone().unwrap_or_default();
        remote.version_behind = newest.get(&kind).is_some_and(|best| version < *best);
    }
}

/// Découpe `8.4.1-2` en `[8, 4, 1]` : les composantes numériques, dans l'ordre.
/// Une version illisible donne une liste vide, qui n'est jamais « en retard ».
fn parse_version(version: &str) -> Vec<u32> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .take(4)
        .collect()
}

pub fn remote_samples(remote: &RemoteView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let kind = remote.kind.clone().unwrap_or_default();
    let mut push = |metric: &str, value: Option<f64>| {
        if let Some(value) = value {
            samples.push(
                gauge(metric, value, ts_ms)
                    .with_label("remote", remote.id.clone())
                    .with_label("type", kind.clone()),
            );
        }
    };

    push("remote_reachable", Some(if remote.reachable { 1.0 } else { 0.0 }));
    push("remote_nodes_online", remote.nodes_online);
    push("remote_nodes_offline", remote.nodes_offline);
    push("remote_guests_running", remote.guests_running);
    push("remote_guests_stopped", remote.guests_stopped);
    push("remote_cpu_used_cores", remote.cpu_used_cores);
    push("remote_cpu_total_cores", remote.cpu_total_cores);
    push("remote_cpu_used_percent", percent(remote.cpu_used_cores, remote.cpu_total_cores));
    push("remote_memory_used_bytes", remote.memory_used_bytes);
    push("remote_memory_total_bytes", remote.memory_total_bytes);
    push(
        "remote_memory_used_percent",
        percent(remote.memory_used_bytes, remote.memory_total_bytes),
    );
    push("remote_storage_used_bytes", remote.storage_used_bytes);
    push("remote_storage_total_bytes", remote.storage_total_bytes);
    push(
        "remote_storage_used_percent",
        percent(remote.storage_used_bytes, remote.storage_total_bytes),
    );
    push("remote_datastores", remote.datastores);
    push("remote_updates_pending", remote.updates_pending);
    // Un abonnement `unknown` ne dit rien : on ne publie que ce qui est su.
    match remote.subscription.as_deref() {
        Some("active") => push("remote_subscription_active", Some(1.0)),
        Some("none") | Some("mixed") => push("remote_subscription_active", Some(0.0)),
        _ => {}
    }
    // La série n'existe que si la version a été lue : une instance injoignable ne
    // doit pas passer pour à jour.
    if remote.version.is_some() {
        push("remote_version_behind", Some(if remote.version_behind { 1.0 } else { 0.0 }));
    }

    if let Some(version) = &remote.version {
        samples.push(
            gauge("remote_version_info", 1.0, ts_ms)
                .with_label("remote", remote.id.clone())
                .with_label("type", kind.clone())
                .with_label("version", version.clone()),
        );
    }
    samples
}

/// Âge de la dernière collecte de métriques d'un remote, en secondes.
///
/// Rendue à part : la date vient d'un appel facultatif, et un âge négatif —
/// horloges désaccordées — ne veut rien dire.
pub fn collection_age_samples(remote: &RemoteView, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let Some(last) = remote.last_collection else { return Vec::new() };
    let age = (now_s - last).max(0) as f64;
    vec![
        gauge("remote_last_collection_age_seconds", age, ts_ms)
            .with_label("remote", remote.id.clone())
            .with_label("type", remote.kind.clone().unwrap_or_default()),
    ]
}

// --------------------------------------------------------------------------
// Les tâches, toutes instances confondues
// --------------------------------------------------------------------------

/// Découpe un `RemoteUpid` : `site-b!UPID:…` donne `("site-b", "UPID:…")`.
/// Un UPID sans préfixe — tâche locale de la console — garde un remote vide.
pub fn split_remote_upid(upid: &str) -> (String, &str) {
    match upid.split_once('!') {
        Some((remote, rest)) if rest.starts_with("UPID:") => (remote.to_string(), rest),
        _ => (String::new(), upid),
    }
}

pub fn task_views(tasks: &[TaskEntry]) -> Vec<TaskView> {
    tasks
        .iter()
        .map(|task| {
            let (remote, _) = split_remote_upid(&task.upid);
            TaskView {
                upid: task.upid.clone(),
                remote,
                worker_type: task.worker_type.clone().unwrap_or_default(),
                worker_id: task.worker_id.clone().unwrap_or_default(),
                node: task.node.clone(),
                user: task.user.clone(),
                start: task.starttime,
                end: task.endtime,
                status: task.status.clone(),
            }
        })
        .collect()
}

/// Vrai pour un statut de tâche qui n'est pas un échec.
pub fn is_success(status: &str) -> bool {
    status.eq_ignore_ascii_case("ok") || status.to_ascii_uppercase().starts_with("WARNINGS")
}

/// Compte les tâches de la fenêtre : totales, en échec, en cours, et en échec
/// par instance.
#[derive(Debug, Default, PartialEq)]
pub struct TaskDigest {
    pub total: usize,
    pub failed: usize,
    pub running: usize,
    pub failed_by_remote: BTreeMap<String, usize>,
}

pub fn digest_tasks(tasks: &[TaskView], now_s: i64, lookback_seconds: i64) -> TaskDigest {
    let floor = now_s - lookback_seconds;
    let mut digest = TaskDigest::default();
    for task in tasks.iter().filter(|task| task.start >= floor) {
        digest.total += 1;
        match task.status.as_deref() {
            None => digest.running += 1,
            Some(status) if is_success(status) => {}
            Some(_) => {
                digest.failed += 1;
                *digest.failed_by_remote.entry(task.remote.clone()).or_default() += 1;
            }
        }
    }
    digest
}

pub fn task_samples(digest: &TaskDigest, remotes: &[RemoteView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        gauge("tasks_total", digest.total as f64, ts_ms),
        gauge("tasks_failed", digest.failed as f64, ts_ms),
        gauge("tasks_running", digest.running as f64, ts_ms),
    ];
    // Une série par instance connue, y compris à zéro : c'est ce qui permet à une
    // règle de voir un échec apparaître, puis disparaître.
    for remote in remotes {
        let failed = digest.failed_by_remote.get(&remote.id).copied().unwrap_or(0);
        samples.push(
            gauge("remote_tasks_failed", failed as f64, ts_ms)
                .with_label("remote", remote.id.clone())
                .with_label("type", remote.kind.clone().unwrap_or_default()),
        );
    }
    samples
}

// --------------------------------------------------------------------------
// L'hôte de la console
// --------------------------------------------------------------------------

pub fn node_view(status: &NodeStatus) -> NodeView {
    NodeView {
        // La charge CPU est un ratio 0..1 ; on l'expose en pourcentage pour rester
        // homogène avec le reste de DumbMonit.
        cpu_percent: num(status.cpu).map(|cpu| cpu * 100.0),
        cpu_count: status.cpuinfo.as_ref().and_then(|info| num(info.cpus)),
        iowait_percent: num(status.wait).map(|wait| wait * 100.0),
        cpu_model: status.cpuinfo.as_ref().and_then(|info| info.model.clone()),
        load1: status.loadavg.first().map(|load| load.0),
        memory_used_bytes: status.memory.as_ref().and_then(|m| num(m.used)),
        memory_total_bytes: status.memory.as_ref().and_then(|m| num(m.total)),
        swap_used_bytes: status.swap.as_ref().and_then(|s| num(s.used)),
        swap_total_bytes: status.swap.as_ref().and_then(|s| num(s.total)),
        rootfs_used_bytes: status.root.as_ref().and_then(|r| num(r.used)),
        rootfs_total_bytes: status.root.as_ref().and_then(|r| num(r.total)),
        uptime_seconds: num(status.uptime),
        kernel: status.kversion.clone(),
        ..Default::default()
    }
}

pub fn node_samples(node: &NodeView, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |metric: &str, value: Option<f64>| {
        if let Some(value) = value {
            samples.push(gauge(metric, value, ts_ms));
        }
    };

    push("node_cpu_percent", node.cpu_percent);
    push("node_cpu_count", node.cpu_count);
    push("node_iowait_percent", node.iowait_percent);
    push("node_load1", node.load1);
    push("node_memory_used_bytes", node.memory_used_bytes);
    push("node_memory_total_bytes", node.memory_total_bytes);
    push("node_memory_used_percent", percent(node.memory_used_bytes, node.memory_total_bytes));
    push("node_swap_used_bytes", node.swap_used_bytes);
    push("node_swap_total_bytes", node.swap_total_bytes);
    push("node_rootfs_used_bytes", node.rootfs_used_bytes);
    push("node_rootfs_total_bytes", node.rootfs_total_bytes);
    push("node_rootfs_percent", percent(node.rootfs_used_bytes, node.rootfs_total_bytes));
    // L'uptime est un `Gauge` : il repart de zéro à chaque redémarrage, et c'est
    // justement cette chute que l'on veut voir telle quelle.
    push("node_uptime_seconds", node.uptime_seconds);
    push("node_updates_pending", node.updates_pending);

    if let Some(kernel) = &node.kernel {
        samples.push(gauge("node_kernel_info", 1.0, ts_ms).with_label("kversion", kernel.clone()));
    }

    for certificate in &node.certificates {
        if let Some(not_after) = certificate.not_after {
            samples.push(
                gauge("node_certificate_expiry_days", (not_after - now_s) as f64 / 86_400.0, ts_ms)
                    .with_label("file", certificate.filename.clone()),
            );
        }
    }

    if let Some(subscription) = &node.subscription {
        if let Some(status) = &subscription.status {
            samples.push(
                gauge(
                    "subscription_active",
                    if status.eq_ignore_ascii_case("active") { 1.0 } else { 0.0 },
                    ts_ms,
                )
                .with_label("status", status.clone()),
            );
        }
        if let Some(active) = subscription.active_nodes {
            samples.push(gauge("subscription_active_nodes", active, ts_ms));
        }
        if let Some(total) = subscription.total_nodes {
            samples.push(gauge("subscription_total_nodes", total, ts_ms));
        }
    }

    samples
}

pub fn certificate_views(certificates: &[CertificateInfo]) -> Vec<CertificateView> {
    certificates
        .iter()
        .map(|certificate| CertificateView {
            filename: certificate.filename.clone().unwrap_or_default(),
            subject: certificate.subject.clone(),
            issuer: certificate.issuer.clone(),
            not_after: certificate.notafter,
        })
        .collect()
}

pub fn subscription_view(subscription: &Subscription) -> SubscriptionView {
    let statistics = subscription.statistics.as_ref();
    SubscriptionView {
        status: subscription.status.clone(),
        message: subscription.message.clone(),
        active_nodes: statistics.and_then(|s| num(s.active_subscriptions)),
        total_nodes: statistics.and_then(|s| num(s.total_nodes)),
    }
}

/// Nombre de paquets en attente sur la console.
pub fn updates_count(updates: &[AptUpdate]) -> f64 {
    updates.len() as f64
}

/// La vue livrée à l'observateur, une fois tout rassemblé.
pub fn build_view(
    probed_at: i64,
    version: Option<String>,
    remotes: Vec<RemoteView>,
    estate: EstateView,
    node: Option<NodeView>,
    tasks: Vec<TaskView>,
) -> ProbeView {
    ProbeView { probed_at, version, remotes, estate, node, tasks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdm::model::Envelope;

    const STATUS: &str = include_str!("testdata/resources_status.json");

    /// `GET /resources/list` d'une console qui fédère un cluster PVE de deux
    /// nœuds avec un stockage partagé, un PBS, et une instance injoignable.
    const RESOURCES: &str = r#"[
      {"remote":"site-a","resources":[
        {"type":"pve-node","id":"remote/site-a/node/pve1","node":"pve1","status":"online","cpu":0.25,"maxcpu":8,"mem":8000000000,"maxmem":16000000000,"uptime":500000},
        {"type":"pve-node","id":"remote/site-a/node/pve2","node":"pve2","status":"offline","cpu":0,"maxcpu":8,"mem":0,"maxmem":16000000000,"uptime":0},
        {"type":"pve-qemu","id":"remote/site-a/guest/100","name":"nextcloud","node":"pve1","status":"running","template":false,"vmid":100,"cpu":0.1,"maxcpu":4,"mem":2000000000,"maxmem":4000000000},
        {"type":"pve-qemu","id":"remote/site-a/guest/101","name":"modele","node":"pve1","status":"stopped","template":true,"vmid":101},
        {"type":"pve-lxc","id":"remote/site-a/guest/200","name":"dns","node":"pve2","status":"stopped","template":false,"vmid":200},
        {"type":"pve-storage","id":"remote/site-a/storage/pve1/shared","node":"pve1","storage":"shared","shared":true,"status":"available","disk":1000,"maxdisk":4000},
        {"type":"pve-storage","id":"remote/site-a/storage/pve2/shared","node":"pve2","storage":"shared","shared":true,"status":"available","disk":1000,"maxdisk":4000},
        {"type":"pve-network","id":"remote/site-a/network/zone1","node":"pve1","network":"zone1","status":"available"}
      ]},
      {"remote":"site-d","resources":[
        {"type":"pbs-node","id":"remote/site-d/node/pbs","name":"pbs","cpu":0.5,"maxcpu":4,"mem":4000000000,"maxmem":8000000000,"uptime":100},
        {"type":"pbs-datastore","id":"remote/site-d/datastore/main","name":"main","disk":500,"maxdisk":2000,"usage":0.25}
      ]},
      {"remote":"site-b","error":"connection refused"}
    ]"#;

    fn status() -> ResourcesStatus {
        serde_json::from_str::<Envelope<ResourcesStatus>>(STATUS).unwrap().data
    }

    fn resources() -> Vec<RemoteResources> {
        serde_json::from_str(RESOURCES).unwrap()
    }

    #[test]
    fn le_parc_entier_tient_dans_une_poignee_de_series() {
        let estate = estate_view(&status());
        assert_eq!(estate.remotes_failed, Some(2.0));
        assert_eq!(estate.qemu_running, Some(0.0));
        let samples = estate_samples(&estate, 0);
        let names: Vec<&str> = samples.iter().map(|s| s.metric.as_str()).collect();
        assert!(names.contains(&"pdm_remotes_failed"));
        assert!(names.contains(&"pdm_guests_running"));
        // Toutes les mesures du parc sont à zéro ici : aucun pourcentage ne doit
        // être publié à partir d'un total nul.
        assert!(!names.contains(&"pdm_memory_used_percent"));
    }

    #[test]
    fn une_instance_injoignable_est_la_seule_a_perdre_ses_mesures() {
        let mut remotes = remote_views(&[], Some(&status()), &resources(), &[], &[]);
        mark_versions_behind(&mut remotes);
        let ids: Vec<&str> = remotes.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["site-a", "site-b", "site-c", "site-d"]);

        let site_a = remotes.iter().find(|r| r.id == "site-a").unwrap();
        assert!(site_a.reachable);
        assert_eq!(site_a.nodes_online, Some(1.0));
        assert_eq!(site_a.nodes_offline, Some(1.0));
        assert_eq!(site_a.guests_running, Some(1.0), "le modèle ne compte pas");
        assert_eq!(site_a.guests_stopped, Some(1.0));
        assert_eq!(site_a.cpu_total_cores, Some(16.0), "les cœurs des nœuds, pas des invités");
        assert_eq!(site_a.cpu_used_cores, Some(2.0));
        assert_eq!(
            site_a.storage_total_bytes,
            Some(4000.0),
            "un stockage partagé n'est compté qu'une fois"
        );

        let site_b = remotes.iter().find(|r| r.id == "site-b").unwrap();
        assert!(!site_b.reachable);
        assert!(site_b.error.as_deref().unwrap().contains("TLS"));
        assert!(site_b.memory_total_bytes.is_none(), "rien à inventer pour une instance muette");

        let site_d = remotes.iter().find(|r| r.id == "site-d").unwrap();
        assert_eq!(site_d.datastores, Some(1.0));
        assert_eq!(site_d.nodes_online, Some(1.0), "un nœud PBS sans statut a répondu");
    }

    #[test]
    fn linstance_en_retard_de_version_est_celle_qui_traine_derriere_ses_semblables() {
        let mut remotes = vec![
            RemoteView {
                id: "a".into(),
                kind: Some("pve".into()),
                version: Some("8.4.1".into()),
                reachable: true,
                ..Default::default()
            },
            RemoteView {
                id: "b".into(),
                kind: Some("pve".into()),
                version: Some("8.2.4".into()),
                reachable: true,
                ..Default::default()
            },
            RemoteView {
                id: "c".into(),
                kind: Some("pbs".into()),
                version: Some("3.2.0".into()),
                reachable: true,
                ..Default::default()
            },
            RemoteView { id: "d".into(), kind: Some("pve".into()), ..Default::default() },
        ];
        mark_versions_behind(&mut remotes);
        assert!(!remotes[0].version_behind);
        assert!(remotes[1].version_behind, "8.2.4 traîne derrière 8.4.1");
        assert!(!remotes[2].version_behind, "seul PBS de son espèce");
        assert!(!remotes[3].version_behind, "sans version, aucun verdict");

        // Sans version, aucune série : une instance muette ne passe pas pour à jour.
        let muette = remote_samples(&remotes[3], 0);
        let names: Vec<&str> = muette.iter().map(|s| s.metric.as_str()).collect();
        assert!(!names.contains(&"pdm_remote_version_behind"));
        let en_retard = remote_samples(&remotes[1], 0);
        let names: Vec<&str> = en_retard.iter().map(|s| s.metric.as_str()).collect();
        assert!(names.contains(&"pdm_remote_version_behind"));
        assert!(names.contains(&"pdm_remote_version_info"));
    }

    #[test]
    fn les_versions_se_comparent_composante_par_composante() {
        assert_eq!(parse_version("8.4.1-2"), vec![8, 4, 1, 2]);
        assert!(parse_version("8.10.0") > parse_version("8.9.9"), "pas une comparaison de texte");
        assert!(parse_version("inconnue").is_empty());
    }

    #[test]
    fn ladresse_dun_noeud_perd_son_empreinte_de_certificat() {
        assert_eq!(strip_fingerprint("10.0.0.1:8006,fingerprint=AB:CD"), "10.0.0.1:8006");
        assert_eq!(strip_fingerprint("pve.lan"), "pve.lan");
    }

    #[test]
    fn lupid_dune_tache_nomme_son_instance() {
        let (remote, upid) = split_remote_upid(
            "site-b!UPID:pve1:0000228F:04457473:00000000:6AB26D84:vzdump::root@pam:",
        );
        assert_eq!(remote, "site-b");
        assert!(upid.starts_with("UPID:"));
        // Une tâche locale de la console n'a pas de préfixe.
        let (remote, upid) = split_remote_upid(
            "UPID:localhost:0000228F:04457473:00000000:6AB26D84:logrotate::root@pam:",
        );
        assert_eq!(remote, "");
        assert!(upid.starts_with("UPID:"));
    }

    #[test]
    fn les_taches_en_echec_se_comptent_par_instance_et_dans_la_fenetre() {
        let tasks: Vec<TaskView> = task_views(
            &serde_json::from_str::<Vec<TaskEntry>>(
                r#"[
                  {"upid":"site-a!UPID:pve1:1:1:1:1:vzdump::root@pam:","worker_type":"vzdump","worker_id":"100","starttime":1000,"endtime":1100,"status":"TASK ERROR: backup failed"},
                  {"upid":"site-a!UPID:pve1:1:1:1:2:vzdump::root@pam:","worker_type":"vzdump","worker_id":"101","starttime":1000,"endtime":1100,"status":"OK"},
                  {"upid":"site-d!UPID:pbs:1:1:1:3:verify::root@pam:","worker_type":"verify","worker_id":"main","starttime":1000,"endtime":1100,"status":"WARNINGS: 2"},
                  {"upid":"site-d!UPID:pbs:1:1:1:4:sync::root@pam:","worker_type":"sync","worker_id":"s1","starttime":1000,"status":null},
                  {"upid":"site-d!UPID:pbs:1:1:1:5:prune::root@pam:","worker_type":"prune","worker_id":"p1","starttime":10,"endtime":20,"status":"TASK ERROR: too old"}
                ]"#,
            )
            .unwrap(),
        );
        let digest = digest_tasks(&tasks, 1200, 600);
        assert_eq!(digest.total, 4, "la tâche de la veille sort de la fenêtre");
        assert_eq!(digest.failed, 1);
        assert_eq!(digest.running, 1);
        assert_eq!(digest.failed_by_remote.get("site-a"), Some(&1));
        assert!(!digest.failed_by_remote.contains_key("site-d"), "WARNINGS n'est pas un échec");

        let remotes = vec![
            RemoteView { id: "site-a".into(), kind: Some("pve".into()), ..Default::default() },
            RemoteView { id: "site-d".into(), kind: Some("pbs".into()), ..Default::default() },
        ];
        let samples = task_samples(&digest, &remotes, 0);
        let per_remote: Vec<f64> = samples
            .iter()
            .filter(|s| s.metric == "pdm_remote_tasks_failed")
            .map(|s| s.value)
            .collect();
        assert_eq!(per_remote, vec![1.0, 0.0], "une série par instance, même à zéro");
    }

    #[test]
    fn letat_de_la_console_ne_fabrique_jamais_de_zero() {
        let status: NodeStatus = serde_json::from_str(
            r#"{"cpu":0.25,"cpuinfo":{"cpus":8,"model":"Core i3"},"loadavg":[0.99,1.8,1.6],
                "memory":{"free":1,"total":16000000000,"used":4000000000},
                "root":{"avail":1,"total":200000000000,"used":180000000000},
                "uptime":716821,"kversion":"Linux 6.8"}"#,
        )
        .unwrap();
        let node = node_view(&status);
        assert_eq!(node.cpu_percent, Some(25.0));
        assert_eq!(node.cpu_count, Some(8.0));
        assert!(node.swap_used_bytes.is_none(), "pas de swap déclaré, pas de série");

        let samples = node_samples(&node, 0, 0);
        let by_name: BTreeMap<&str, f64> =
            samples.iter().map(|s| (s.metric.as_str(), s.value)).collect();
        assert_eq!(by_name.get("pdm_node_memory_used_percent"), Some(&25.0));
        assert_eq!(by_name.get("pdm_node_rootfs_percent"), Some(&90.0));
        assert!(!by_name.contains_key("pdm_node_swap_used_bytes"));
        assert!(!by_name.contains_key("pdm_node_updates_pending"), "droit absent : aucune série");
    }

    #[test]
    fn un_certificat_donne_ses_jours_restants() {
        let certificates: Vec<CertificateInfo> = serde_json::from_str(
            r#"[{"filename":"proxy.pem","issuer":"CN = dc","notafter":864000,"subject":"CN = dc"}]"#,
        )
        .unwrap();
        let node =
            NodeView { certificates: certificate_views(&certificates), ..Default::default() };
        let samples = node_samples(&node, 0, 0);
        let expiry =
            samples.iter().find(|s| s.metric == "pdm_node_certificate_expiry_days").unwrap();
        assert_eq!(expiry.value, 10.0);
        assert_eq!(expiry.labels.get("file").map(String::as_str), Some("proxy.pem"));
    }

    #[test]
    fn labonnement_du_parc_se_lit_dans_ses_statistiques() {
        let subscription: Subscription = serde_json::from_str(
            r#"{"message":"Too many remote nodes without active basic or higher subscription!",
                "statistics":{"active-subscriptions":0,"community":0,"total-nodes":2},
                "status":"invalid","url":"https://pdm.proxmox.com/faq.html"}"#,
        )
        .unwrap();
        let view = subscription_view(&subscription);
        assert_eq!(view.status.as_deref(), Some("invalid"));
        assert_eq!(view.total_nodes, Some(2.0));

        let node = NodeView { subscription: Some(view), ..Default::default() };
        let samples = node_samples(&node, 0, 0);
        let active = samples.iter().find(|s| s.metric == "pdm_subscription_active").unwrap();
        assert_eq!(active.value, 0.0);
        assert_eq!(active.labels.get("status").map(String::as_str), Some("invalid"));
    }

    #[test]
    fn lage_de_la_derniere_collecte_ne_devient_jamais_negatif() {
        let remote = RemoteView {
            id: "site-a".into(),
            kind: Some("pve".into()),
            last_collection: Some(1_000),
            ..Default::default()
        };
        assert_eq!(collection_age_samples(&remote, 1_600, 0)[0].value, 600.0);
        assert_eq!(collection_age_samples(&remote, 900, 0)[0].value, 0.0);
        let muet = RemoteView { id: "b".into(), ..Default::default() };
        assert!(collection_age_samples(&muet, 1_600, 0).is_empty());
    }
}
