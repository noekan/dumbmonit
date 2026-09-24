//! Paquets, dépôts et abonnement d'un nœud : `apt/update`, `apt/versions`,
//! `apt/repositories` et `subscription`.
//!
//! Deux questions d'administrateur y trouvent réponse sans qu'une seule série
//! par paquet soit créée :
//!
//! * **y a-t-il des correctifs de sécurité en attente ?** — comptés à part des
//!   autres mises à jour, car ce sont les seuls qui justifient une notification ;
//! * **quelqu'un a-t-il mis à jour ce nœud ?** — `apt/versions` donne la version
//!   installée des paquets importants de Proxmox ; la comparer à celle de
//!   l'interrogation précédente révèle un `apt upgrade` passé (ou un
//!   `dist-upgrade` qu'on n'attendait pas). Le changement est publié pendant
//!   [`CHANGE_HOLD_SECONDS`] avec son résumé en étiquette, assez longtemps pour
//!   qu'une règle le voie et le notifie, puis la série revient à zéro.

use std::collections::BTreeMap;

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{AptPackage, AptVersion, Repositories, Subscription};

/// Durée pendant laquelle un changement de paquets reste publié.
///
/// Le moteur d'alerte lit un instantané toutes les trente secondes et exige une
/// durée minimale : une heure laisse la règle se déclencher, notifier, puis se
/// résoudre d'elle-même sans que rien ne traîne.
pub const CHANGE_HOLD_SECONDS: i64 = 3600;

/// Longueur maximale du résumé des changements porté en étiquette.
///
/// Une étiquette est faite pour une ligne de notification, pas pour un journal :
/// au-delà, on compte le reste plutôt que de le citer.
const SUMMARY_MAX_CHARS: usize = 200;

/// Vrai pour une mise à jour issue d'un dépôt de sécurité.
///
/// PVE ne dit pas de quel dépôt vient un paquet, seulement son `Origin`
/// (« Debian », « Proxmox »). On reconnaît donc le dépôt de sécurité de Debian à
/// tout ce qui le nomme : l'origine ou l'étiquette du dépôt (`Debian-Security`),
/// la suite (`bookworm-security`), l'URL du journal des changements, ou une
/// section `security`. Un paquet Debian sans aucun de ces indices est compté
/// comme une mise à jour ordinaire.
pub fn is_security_update(package: &AptPackage) -> bool {
    [
        package.origin.as_deref(),
        package.label.as_deref(),
        package.suite.as_deref(),
        package.archive.as_deref(),
        package.section.as_deref(),
        package.changelog_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_ascii_lowercase().contains("security"))
}

/// `GET /nodes/{node}/apt/update` : le nombre de paquets en attente, et parmi
/// eux ceux de sécurité.
///
/// Le détail des paquets n'est pas repris : une étiquette par paquet ferait
/// autant de séries que de mises à jour, pour une information qui change à
/// chaque publication de Proxmox.
pub fn updates_samples(node: &str, packages: &[AptPackage], ts_ms: i64) -> Vec<Sample> {
    let security = packages.iter().filter(|package| is_security_update(package)).count();
    vec![
        gauge("node_updates_pending", packages.len() as f64, ts_ms).with_label("node", node),
        gauge("node_updates_security_pending", security as f64, ts_ms).with_label("node", node),
    ]
}

/// `GET /nodes/{node}/apt/versions` : ce que la liste des versions dit sans
/// `Sys.Modify`.
///
/// `apt/update` — le seul endpoint qui compte les mises à jour en attente —
/// exige `Sys.Modify`, un droit d'écriture que beaucoup n'accordent pas à un
/// jeton de supervision : sur un tel cluster, `node_updates_pending` n'existe
/// tout simplement pas. `apt/versions`, lui, ne demande que `Sys.Audit`, et
/// porte déjà la version candidate de chaque paquet Proxmox : de quoi dire
/// qu'une mise à jour attend, à défaut de la compter sur tout le système.
///
/// Même appel, même droit : le noyau en marche y figure aussi, ce qui permet de
/// dire qu'un noyau plus récent est installé mais pas encore démarré — la seule
/// façon de savoir qu'un nœud attend son redémarrage.
pub fn version_samples(node: &str, versions: &[AptVersion], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("node", node));

    let upgradable = versions.iter().filter(|version| version.is_upgradable()).count();
    push(gauge("node_pve_packages_upgradable", upgradable as f64, ts_ms));

    let running = versions
        .iter()
        .find_map(|version| version.running_kernel.as_deref().filter(|kernel| !kernel.is_empty()));
    if let Some(running) = running {
        let newest = versions
            .iter()
            .filter_map(installed_kernel)
            .max_by(|a, b| version_key(a).cmp(&version_key(b)));
        let pending = newest
            .filter(|newest| version_key(newest) > version_key(&kernel_version(running)))
            .unwrap_or("");
        push(
            gauge("node_reboot_required", if pending.is_empty() { 0.0 } else { 1.0 }, ts_ms)
                .with_label("running", running)
                .with_label("installed", pending),
        );
    }

    samples
}

/// Version du noyau portée par un paquet `proxmox-kernel-…` installé.
///
/// Les métapaquets (`proxmox-kernel-7.0`, `proxmox-kernel-helper`) sont écartés :
/// ils suivent la série, pas l'image réellement présente sur le disque.
fn installed_kernel(version: &AptVersion) -> Option<&str> {
    let name = version.package.as_deref()?;
    version.installed()?;
    let rest = name.strip_prefix("proxmox-kernel-").or_else(|| name.strip_prefix("pve-kernel-"))?;
    let rest = rest.strip_suffix("-pve-signed").or_else(|| rest.strip_suffix("-pve"))?;
    rest.starts_with(|c: char| c.is_ascii_digit()).then_some(rest)
}

/// Le noyau en marche sans son suffixe de saveur : `7.0.6-2-pve` → `7.0.6-2`.
fn kernel_version(running: &str) -> String {
    running.strip_suffix("-pve").unwrap_or(running).to_string()
}

/// Découpe une version en nombres comparables : `7.0.14-11` passe ainsi devant
/// `7.0.6-2`, ce qu'un ordre lexicographique ferait faux.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// Un changement de versions constaté entre deux interrogations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageChange {
    /// Date de détection, en secondes Unix.
    pub detected_at: i64,
    /// Nombre de paquets dont la version installée a changé.
    pub count: u32,
    /// `pve-manager 8.2.4→8.2.7, pve-kernel-6.8 6.8.8-2→6.8.12-2`, tronqué.
    pub summary: String,
}

/// Ce que le collecteur retient d'un nœud entre deux interrogations.
#[derive(Debug, Default, Clone)]
pub struct PackageMemory {
    /// Version installée par paquet, à la dernière interrogation.
    versions: BTreeMap<String, String>,
    /// Dernier changement constaté, tant qu'il est encore publié.
    change: Option<PackageChange>,
}

impl PackageMemory {
    /// Compare la liste courante à la précédente et retient le changement.
    ///
    /// La première interrogation ne compare à rien : elle ne fait qu'apprendre.
    /// Un paquet nouvellement installé ou retiré n'est pas un changement de
    /// version — seul compte un paquet présent aux deux interrogations dont la
    /// version a bougé, ce qui est exactement ce qu'un `apt upgrade` produit.
    pub fn observe(&mut self, versions: &[AptVersion], now_s: i64) -> Option<&PackageChange> {
        let current: BTreeMap<String, String> = versions
            .iter()
            .filter_map(|entry| Some((entry.package.clone()?, entry.installed()?.to_string())))
            .collect();

        if !self.versions.is_empty() {
            let mut changed: Vec<String> = Vec::new();
            for (package, version) in &current {
                if let Some(previous) = self.versions.get(package)
                    && previous != version
                {
                    changed.push(format!("{package} {previous}→{version}"));
                }
            }
            if !changed.is_empty() {
                let count = changed.len() as u32;
                self.change =
                    Some(PackageChange { detected_at: now_s, count, summary: summarize(&changed) });
            }
        }
        self.versions = current;

        // Un changement expiré n'est plus publié ; l'oublier libère la série.
        if self
            .change
            .as_ref()
            .is_some_and(|change| now_s - change.detected_at >= CHANGE_HOLD_SECONDS)
        {
            self.change = None;
        }
        self.change.as_ref()
    }

    /// Nombre de paquets suivis.
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.versions.len()
    }
}

/// Résumé sur une ligne, borné à [`SUMMARY_MAX_CHARS`].
fn summarize(changed: &[String]) -> String {
    let mut summary = String::new();
    for (index, item) in changed.iter().enumerate() {
        let candidate =
            if summary.is_empty() { item.clone() } else { format!("{summary}, {item}") };
        if candidate.chars().count() > SUMMARY_MAX_CHARS {
            let rest = changed.len() - index;
            summary.push_str(&format!(" +{rest} more"));
            return summary;
        }
        summary = candidate;
    }
    summary
}

/// `GET /nodes/{node}/apt/versions` : la série de changement.
///
/// Vaut le nombre de paquets changés tant que le changement est publié, avec
/// le résumé en étiquette `changes` ; zéro sinon, sans étiquette — ce sont deux
/// séries distinctes, la seconde restant stable d'une interrogation à l'autre.
pub fn package_change_samples(
    node: &str,
    change: Option<&PackageChange>,
    ts_ms: i64,
) -> Vec<Sample> {
    match change {
        Some(change) => vec![
            gauge("node_packages_changed", f64::from(change.count), ts_ms)
                .with_label("node", node)
                .with_label("changes", change.summary.clone()),
        ],
        None => vec![gauge("node_packages_changed", 0.0, ts_ms).with_label("node", node)],
    }
}

/// `GET /nodes/{node}/subscription`.
///
/// `active` vaut 1 pour un abonnement en cours ; tout le reste (`notfound` sur un
/// homelab, `expired`, `invalid`) vaut 0. L'état lui-même voyage en étiquette
/// d'une série `_info`, pour que l'interface puisse l'écrire en toutes lettres.
pub fn subscription_samples(node: &str, subscription: &Subscription, ts_ms: i64) -> Vec<Sample> {
    let status = subscription.status.clone().unwrap_or_else(|| "unknown".to_string());
    let active = status.eq_ignore_ascii_case("active");
    vec![
        gauge("node_subscription_active", if active { 1.0 } else { 0.0 }, ts_ms)
            .with_label("node", node),
        gauge("node_subscription_info", 1.0, ts_ms)
            .with_label("node", node)
            .with_label("status", status)
            .with_label("level", subscription.level.clone().unwrap_or_default())
            .with_label("next_due", subscription.nextduedate.clone().unwrap_or_default()),
    ]
}

/// `GET /nodes/{node}/apt/repositories`.
///
/// Ce qui compte pour l'exploitation : un fichier de sources illisible (le
/// nœud ne se met plus à jour), un avertissement de Proxmox (dépôt entreprise
/// sans abonnement, dépôt de test en production), et quel dépôt standard est
/// actif.
pub fn repository_samples(node: &str, repositories: &Repositories, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("node", node));

    push(gauge("node_repository_errors", repositories.errors.len() as f64, ts_ms));
    let warnings =
        repositories.infos.iter().filter(|info| info.kind.as_deref() == Some("warning")).count();
    push(gauge("node_repository_warnings", warnings as f64, ts_ms));

    for repo in &repositories.standard_repos {
        let Some(status) = repo.status else { continue };
        push(
            gauge("node_repository_enabled", if status.0 != 0.0 { 1.0 } else { 0.0 }, ts_ms)
                .with_label("repo", repo.handle.clone()),
        );
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// `GET /nodes/pve1/apt/update` : trois paquets Proxmox et un correctif
    /// de sécurité Debian.
    const APT_UPDATES: &str = r#"{"data":[
      {"Package":"pve-manager","Title":"Proxmox Virtual Environment Management Tools","Description":"Proxmox Virtual Environment Management Tools\n","Section":"admin","Priority":"optional","Origin":"Proxmox","Arch":"amd64","OldVersion":"8.2.4","Version":"8.2.7","ChangeLogUrl":"https://enterprise.proxmox.com/debian/pve/pve-manager"},
      {"Package":"pve-kernel-6.8","Title":"Latest Proxmox VE Kernel Image","Description":"Latest Proxmox VE Kernel Image\n","Section":"admin","Priority":"optional","Origin":"Proxmox","Arch":"amd64","OldVersion":"6.8.8-2","Version":"6.8.12-2","ChangeLogUrl":"https://enterprise.proxmox.com/debian/pve/pve-kernel-6.8"},
      {"Package":"libpve-common-perl","Title":"Proxmox VE base library","Description":"Proxmox VE base library\n","Section":"admin","Priority":"optional","Origin":"Proxmox","Arch":"amd64","OldVersion":"8.2.1","Version":"8.2.3","ChangeLogUrl":"https://enterprise.proxmox.com/debian/pve/libpve-common-perl"},
      {"Package":"openssl","Title":"Secure Sockets Layer toolkit - cryptographic utility","Description":"...","Section":"utils","Priority":"optional","Origin":"Debian","Label":"Debian-Security","Suite":"bookworm-security","Arch":"amd64","OldVersion":"3.0.13-1~deb12u1","Version":"3.0.14-1~deb12u2","ChangeLogUrl":"https://metadata.ftp-master.debian.org/changelogs/main/o/openssl/openssl_3.0.14-1~deb12u2_changelog"}
    ]}"#;

    /// `GET /nodes/pve1/apt/versions` : les paquets importants, `OldVersion`
    /// étant la version installée.
    const APT_VERSIONS_BEFORE: &str = r#"{"data":[
      {"Package":"pve-manager","Version":"8.2.7","OldVersion":"8.2.4","CurrentState":"Installed","ManagerVersion":"8.2.4","RunningKernel":"6.8.8-2-pve","Title":"Proxmox Virtual Environment Management Tools","Origin":"Proxmox","Section":"admin","Priority":"optional","Arch":"amd64"},
      {"Package":"proxmox-kernel-6.8","Version":"6.8.12-2","OldVersion":"6.8.8-2","CurrentState":"Installed","Title":"Latest Proxmox Kernel Image","Origin":"Proxmox","Section":"admin"},
      {"Package":"qemu-server","Version":"8.2.1","OldVersion":"8.2.1","CurrentState":"Installed","Title":"Qemu Server Tools","Origin":"Proxmox","Section":"admin"},
      {"Package":"zfsutils-linux","Version":"2.2.4-pve1","OldVersion":"2.2.4-pve1","CurrentState":"Installed","Origin":"Proxmox"},
      {"Package":"ceph","Version":"18.2.2-pve1","CurrentState":"NotInstalled","Origin":"Proxmox"}
    ]}"#;

    /// La même liste après `apt full-upgrade` : deux paquets ont changé.
    const APT_VERSIONS_AFTER: &str = r#"{"data":[
      {"Package":"pve-manager","Version":"8.2.7","OldVersion":"8.2.7","CurrentState":"Installed","ManagerVersion":"8.2.7","RunningKernel":"6.8.8-2-pve"},
      {"Package":"proxmox-kernel-6.8","Version":"6.8.12-2","OldVersion":"6.8.12-2","CurrentState":"Installed"},
      {"Package":"qemu-server","Version":"8.2.1","OldVersion":"8.2.1","CurrentState":"Installed"},
      {"Package":"zfsutils-linux","Version":"2.2.4-pve1","OldVersion":"2.2.4-pve1","CurrentState":"Installed"},
      {"Package":"ceph","Version":"18.2.2-pve1","OldVersion":"18.2.2-pve1","CurrentState":"Installed"}
    ]}"#;

    /// `GET /nodes/pve1/subscription` d'un homelab sans abonnement.
    const SUBSCRIPTION_NONE: &str = r#"{"data":{"status":"notfound","message":"There is no subscription key","serverid":"AB12CD34EF56AB12CD34EF56AB12CD34","url":"https://www.proxmox.com/en/proxmox-virtual-environment/pricing"}}"#;

    /// Le même appel avec un abonnement communautaire.
    const SUBSCRIPTION_ACTIVE: &str = r#"{"data":{"status":"active","level":"c","key":"pve1c-0123456789","productname":"Proxmox VE Community Subscription 1 CPU/year","nextduedate":"2027-03-01","checktime":1789510633,"regdate":"2026-03-01 00:00:00","serverid":"AB12CD34EF56AB12CD34EF56AB12CD34","sockets":1,"url":"https://www.proxmox.com/en/proxmox-virtual-environment/pricing"}}"#;

    /// `GET /nodes/pve1/apt/repositories` : entreprise désactivé, no-subscription
    /// activé, un avertissement, un fichier illisible.
    const REPOSITORIES: &str = r#"{"data":{
      "digest":"a1b2c3",
      "files":[{"path":"/etc/apt/sources.list","file-type":"list","repositories":[{"Enabled":1,"Types":["deb"],"URIs":["http://deb.debian.org/debian"],"Suites":["bookworm"],"Components":["main","contrib"],"FileType":"list"}]}],
      "errors":[{"path":"/etc/apt/sources.list.d/broken.list","error":"malformed entry 3 in list file"}],
      "infos":[
        {"path":"/etc/apt/sources.list.d/pve-no-subscription.list","index":0,"property":"Suites","kind":"badge","message":"The no-subscription repository is not recommended for production use!"},
        {"path":"/etc/apt/sources.list.d/pve-enterprise.list","index":0,"property":"URIs","kind":"warning","message":"The enterprise repository is enabled, but there is no active subscription!"}
      ],
      "standard-repos":[
        {"handle":"enterprise","name":"Enterprise","description":"This is the default, stable, and recommended repository, available for all Proxmox subscription users.","status":0},
        {"handle":"no-subscription","name":"No-Subscription","description":"This is the recommended repository for testing and non-production use.","status":1},
        {"handle":"test","name":"Test","description":"This repository contains the latest packages and is primarily used for test labs and by developers to test new features."},
        {"handle":"ceph-quincy-enterprise","name":"Ceph Quincy Enterprise","description":"This repository holds the production-ready Proxmox Ceph Quincy packages."}
      ]
    }}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn les_mises_a_jour_en_attente_sont_comptees_par_noeud_avec_celles_de_securite() {
        let packages: Vec<AptPackage> = extraire(APT_UPDATES);
        let samples = updates_samples("pve1", &packages, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_updates_pending{node="pve1"}"#), Some(4.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_node_updates_security_pending{node="pve1"}"#),
            Some(1.0)
        );
        let vide = updates_samples("pve2", &[], 1000);
        assert_eq!(valeur(&vide, r#"proxmox_node_updates_pending{node="pve2"}"#), Some(0.0));
        assert_eq!(
            valeur(&vide, r#"proxmox_node_updates_security_pending{node="pve2"}"#),
            Some(0.0)
        );
    }

    #[test]
    fn un_correctif_de_securite_se_reconnait_a_son_depot_ou_a_son_journal() {
        let etiquette: AptPackage =
            serde_json::from_str(r#"{"Package":"a","Origin":"Debian","Label":"Debian-Security"}"#)
                .unwrap();
        assert!(is_security_update(&etiquette));
        let journal: AptPackage = serde_json::from_str(
            r#"{"Package":"b","Origin":"Debian","ChangeLogUrl":"https://security-tracker.debian.org/x"}"#,
        )
        .unwrap();
        assert!(is_security_update(&journal));
        let ordinaire: AptPackage =
            serde_json::from_str(r#"{"Package":"c","Origin":"Debian","Section":"utils"}"#).unwrap();
        assert!(!is_security_update(&ordinaire));
        let proxmox: AptPackage =
            serde_json::from_str(r#"{"Package":"d","Origin":"Proxmox"}"#).unwrap();
        assert!(!is_security_update(&proxmox));
    }

    #[test]
    fn la_premiere_interrogation_apprend_sans_signaler() {
        let before: Vec<AptVersion> = extraire(APT_VERSIONS_BEFORE);
        let mut memory = PackageMemory::default();
        assert!(memory.observe(&before, 1000).is_none());
        assert_eq!(memory.tracked(), 4, "le paquet non installé n'est pas suivi");
        // Rien ne bouge : toujours rien.
        assert!(memory.observe(&before, 1060).is_none());
    }

    #[test]
    fn un_changement_de_version_est_signale_puis_oublie_apres_la_retenue() {
        let before: Vec<AptVersion> = extraire(APT_VERSIONS_BEFORE);
        let after: Vec<AptVersion> = extraire(APT_VERSIONS_AFTER);
        let mut memory = PackageMemory::default();
        memory.observe(&before, 1000);

        let change = memory.observe(&after, 1060).expect("deux paquets ont changé").clone();
        assert_eq!(change.count, 2, "ceph, nouvellement installé, n'est pas un changement");
        assert_eq!(change.summary, "proxmox-kernel-6.8 6.8.8-2→6.8.12-2, pve-manager 8.2.4→8.2.7");

        // Le changement reste publié pendant la retenue, puis disparaît.
        assert_eq!(memory.observe(&after, 1060 + CHANGE_HOLD_SECONDS - 1), Some(&change));
        assert!(memory.observe(&after, 1060 + CHANGE_HOLD_SECONDS).is_none());

        let samples = package_change_samples("pve1", Some(&change), 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value, 2.0);
        assert_eq!(samples[0].labels["changes"], change.summary);
        let calme = package_change_samples("pve1", None, 1000);
        assert_eq!(valeur(&calme, r#"proxmox_node_packages_changed{node="pve1"}"#), Some(0.0));
    }

    #[test]
    fn le_resume_des_changements_est_borne() {
        let changed: Vec<String> =
            (0..40).map(|i| format!("package-{i:02} 1.0.{i}→1.0.{}", i + 1)).collect();
        let summary = summarize(&changed);
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS + 12, "{summary}");
        assert!(summary.ends_with("more"), "{summary}");
        assert!(summary.starts_with("package-00 1.0.0→1.0.1, package-01"));
    }

    #[test]
    fn labonnement_absent_vaut_zero_et_garde_son_etat_en_etiquette() {
        let none: Subscription = extraire(SUBSCRIPTION_NONE);
        let samples = subscription_samples("pve1", &none, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_subscription_active{node="pve1"}"#), Some(0.0));
        let info = samples.iter().find(|s| s.metric == "proxmox_node_subscription_info").unwrap();
        assert_eq!(info.labels["status"], "notfound");

        let active: Subscription = extraire(SUBSCRIPTION_ACTIVE);
        let samples = subscription_samples("pve1", &active, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_subscription_active{node="pve1"}"#), Some(1.0));
        let info = samples.iter().find(|s| s.metric == "proxmox_node_subscription_info").unwrap();
        assert_eq!(info.labels["level"], "c");
        assert_eq!(info.labels["next_due"], "2027-03-01");
        assert!(
            samples.iter().all(|s| !s.labels.values().any(|v| v.contains("pve1c-0123456789"))),
            "la clé d'abonnement ne sort jamais"
        );
    }

    #[test]
    fn les_depots_donnent_leurs_erreurs_leurs_avertissements_et_leur_etat() {
        let repos: Repositories = extraire(REPOSITORIES);
        let samples = repository_samples("pve1", &repos, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_repository_errors{node="pve1"}"#), Some(1.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_node_repository_warnings{node="pve1"}"#),
            Some(1.0),
            "le badge n'est pas un avertissement"
        );
        assert_eq!(
            valeur(&samples, r#"proxmox_node_repository_enabled{node="pve1",repo="enterprise"}"#),
            Some(0.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_repository_enabled{node="pve1",repo="no-subscription"}"#
            ),
            Some(1.0)
        );
        assert!(
            samples.iter().all(|s| s.labels.get("repo").map(String::as_str) != Some("test")),
            "un dépôt non configuré ne fait pas de série"
        );
    }
}
