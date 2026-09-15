//! Santé d'un système Linux : distribution, mises à jour en attente, redémarrage
//! requis, unités systemd en échec, SELinux.
//!
//! Pensé pour Fedora Server et sa famille (`dnf`), avec Debian et Ubuntu (`apt`)
//! en repli quand cela ne coûte rien. Ailleurs — Alpine, Windows — le module se
//! tait : mieux vaut aucune série qu'un zéro qui laisserait croire que tout est à
//! jour.
//!
//! Deux rythmes cohabitent. L'inventaire des mises à jour charge toutes les
//! métadonnées de dépôt (une à deux secondes et près de 200 Mo pour `dnf` sur
//! Fedora) : il tourne dans une tâche de fond, au plus une fois par
//! `updates_interval_secs`, et son dernier résultat est réémis à chaque cycle.
//! Les autres lectures — un fichier dans `/sys`, une ligne de `systemctl` —
//! tiennent en quelques millisecondes et sont refaites à chaque cycle.
//!
//! Aucune commande n'est lancée par un interpréteur, chacune est bornée par un
//! délai strict, et sa sortie passe par une fonction pure vérifiée sur des
//! sorties réelles : c'est le seul endroit où un changement de format de `dnf`
//! peut nous surprendre, autant qu'il soit couvert.

// Hors Linux, seules les structures et les fonctions pures sont compilées : les
// tests tournent partout, l'exécution de commandes n'a de sens que sur Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeSet;

use ezymonit_proto::{MetricKind, Sample};

/// Plafond de séries `agent_systemd_unit_failed` : au-delà, une machine en
/// perdition ferait exploser le nombre de séries sans rien apprendre de plus que
/// le compteur global.
pub const MAX_FAILED_UNIT_SERIES: usize = 50;

/// Identité de la distribution, lue dans `/etc/os-release`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsInfo {
    pub id: String,
    pub name: String,
    pub version_id: String,
    pub pretty_name: String,
    pub id_like: String,
}

/// Mode SELinux, dans l'ordre croissant de contrainte : la valeur remontée suit
/// cet ordre, ce qui permet une règle « `< 2` » pour détecter un relâchement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelinuxMode {
    Disabled,
    Permissive,
    Enforcing,
}

impl SelinuxMode {
    pub fn as_value(self) -> f64 {
        match self {
            Self::Disabled => 0.0,
            Self::Permissive => 1.0,
            Self::Enforcing => 2.0,
        }
    }
}

/// Mises à jour en attente, telles que le gestionnaire de paquets les voit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatesStat {
    pub pending: u64,
    /// `None` : le gestionnaire n'a pas su isoler les correctifs de sécurité
    /// (dépôt sans métadonnées d'avis, commande absente).
    pub security: Option<u64>,
}

/// Photographie de la santé du système. Chaque champ absent signifie
/// « indéterminable ici », et ne produit aucune série.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemHealthStat {
    pub os: Option<OsInfo>,
    pub updates: Option<UpdatesStat>,
    pub reboot_required: Option<bool>,
    pub failed_units: Option<Vec<String>>,
    pub selinux: Option<SelinuxMode>,
}

/// Traduit la photographie en échantillons.
pub fn samples(stat: &SystemHealthStat, now_ms: i64) -> Vec<Sample> {
    let gauge = |metric: &str, value: f64| Sample::new(metric, value, MetricKind::Gauge, now_ms);
    let mut samples = Vec::with_capacity(8);

    if let Some(os) = &stat.os {
        let mut sample = gauge("agent_os_info", 1.0);
        for (key, value) in [
            ("id", &os.id),
            ("name", &os.name),
            ("version_id", &os.version_id),
            ("pretty_name", &os.pretty_name),
            ("id_like", &os.id_like),
        ] {
            // Une étiquette vide n'identifie rien : Fedora n'a pas d'`ID_LIKE`, et
            // la poser vide ne ferait qu'alourdir la clé de série.
            if !value.is_empty() {
                sample = sample.with_label(key, value);
            }
        }
        samples.push(sample);
    }

    if let Some(updates) = &stat.updates {
        samples.push(gauge("agent_updates_pending", updates.pending as f64));
        if let Some(security) = updates.security {
            samples.push(gauge("agent_security_updates_pending", security as f64));
        }
    }

    if let Some(required) = stat.reboot_required {
        samples.push(gauge("agent_reboot_required", f64::from(u8::from(required))));
    }

    if let Some(units) = &stat.failed_units {
        samples.push(gauge("agent_systemd_failed_units", units.len() as f64));
        for unit in units.iter().take(MAX_FAILED_UNIT_SERIES) {
            samples.push(gauge("agent_systemd_unit_failed", 1.0).with_label("unit", unit));
        }
    }

    if let Some(mode) = stat.selinux {
        samples.push(gauge("agent_selinux_mode", mode.as_value()));
    }

    samples
}

// ----------------------------------------------------------------- analyse

/// Lit `/etc/os-release` : des paires `CLÉ=valeur`, la valeur éventuellement
/// entre guillemets, avec les échappements minimaux que la spécification impose.
pub fn parse_os_release(text: &str) -> OsInfo {
    let mut os = OsInfo::default();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else { continue };
        let value = unquote(raw.trim());
        match key.trim() {
            "ID" => os.id = value,
            "NAME" => os.name = value,
            "VERSION_ID" => os.version_id = value,
            "PRETTY_NAME" => os.pretty_name = value,
            "ID_LIKE" => os.id_like = value,
            _ => {}
        }
    }
    os
}

fn unquote(raw: &str) -> String {
    let quoted = raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')));
    let inner = if quoted { &raw[1..raw.len() - 1] } else { raw };

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // `\"`, `\\`, `\$`, `` \` `` : on garde le caractère protégé.
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Compte les paquets listés par `dnf check-update`.
///
/// Code 100 : des mises à jour existent, une par ligne commençant en colonne 1
/// par `nom.arch`. Code 0 : rien à faire. Tout autre code — cache absent, dépôt
/// illisible — est une réponse inconnue, pas un zéro. Les sections
/// « Obsoleting Packages » (dnf 4) et « Obsoletes » (dnf 5) sont ignorées : un
/// paquet remplacé y figure une seconde fois.
pub fn parse_dnf_check_update(stdout: &str, exit_code: Option<i32>) -> Option<u64> {
    match exit_code {
        Some(0) => return Some(0),
        Some(100) => {}
        _ => return None,
    }
    let mut count = 0;
    for line in stdout.lines() {
        if line.starts_with("Obsolet") {
            break;
        }
        if is_dnf_package_line(line) {
            count += 1;
        }
    }
    Some(count)
}

/// Une ligne de paquet commence en colonne 1 par `nom.arch` ; dnf 4 replie les
/// noms trop longs sur deux lignes, la seconde commençant par des espaces.
fn is_dnf_package_line(line: &str) -> bool {
    if line.is_empty() || line.starts_with(char::is_whitespace) {
        return false;
    }
    let first = line.split_whitespace().next().unwrap_or("");
    let Some((name, arch)) = first.rsplit_once('.') else { return false };
    !name.is_empty() && !arch.is_empty() && !first.ends_with(':')
}

/// Compte les paquets touchés par un avis de sécurité (`dnf updateinfo list
/// --security`), sans doublon : un même paquet peut relever de plusieurs avis.
///
/// dnf 5 écrit `AVIS security Sévérité paquet date heure` sous une ligne
/// d'en-tête ; dnf 4 écrit `AVIS Sévérité/Sec. paquet` sans en-tête.
pub fn parse_dnf_updateinfo_security(stdout: &str) -> u64 {
    let mut packages = BTreeSet::new();
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let kind = fields[1].to_ascii_lowercase();
        if !kind.contains("sec") {
            continue;
        }
        let package =
            if kind.contains('/') { fields[2] } else { fields.get(3).copied().unwrap_or("") };
        if !package.is_empty() {
            packages.insert(package.to_string());
        }
    }
    packages.len() as u64
}

/// Lit `apt list --upgradable` : une ligne par paquet, la suite d'origine entre
/// le `/` et l'espace. Les correctifs de sécurité viennent d'une suite
/// `*-security` (`bookworm-security`, `noble-security`).
pub fn parse_apt_upgradable(stdout: &str) -> UpdatesStat {
    let mut stat = UpdatesStat { pending: 0, security: Some(0) };
    for line in stdout.lines() {
        if !line.contains("[upgradable from:") {
            continue;
        }
        stat.pending += 1;
        let suite = line.split_whitespace().next().and_then(|first| first.split_once('/'));
        if suite.is_some_and(|(_, suite)| suite.split(',').any(|s| s.ends_with("-security"))) {
            stat.security = Some(stat.security.unwrap_or(0) + 1);
        }
    }
    stat
}

/// Interprète `dnf needs-restarting -r`.
///
/// Le texte prime sur le code de retour : dnf 4 renvoie 1 aussi bien pour
/// « redémarrage requis » que pour « commande inconnue », et seule la phrase
/// permet de les distinguer.
pub fn parse_needs_restarting(stdout: &str, exit_code: Option<i32>) -> Option<bool> {
    let text = stdout.to_ascii_lowercase();
    if text.contains("should not be necessary") {
        return Some(false);
    }
    if text.contains("reboot is required") {
        return Some(true);
    }
    match exit_code {
        Some(0) => Some(false),
        _ => None,
    }
}

/// Lit `systemctl --failed --plain --no-legend` : le nom de l'unité est en
/// première colonne, le reste (état, description) n'apporte rien à une alerte.
pub fn parse_failed_units(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let first = fields.next()?;
            // Sans `--plain`, ou avec une vieille version, la ligne commence par
            // une puce : on prend alors la colonne suivante.
            let unit = if first == "●" || first == "*" { fields.next()? } else { first };
            (unit.contains('.')).then(|| unit.to_string())
        })
        .collect()
}

/// `/sys/fs/selinux/enforce` : `1` en application, `0` permissif.
pub fn parse_selinux_enforce(text: &str) -> Option<SelinuxMode> {
    match text.trim() {
        "1" => Some(SelinuxMode::Enforcing),
        "0" => Some(SelinuxMode::Permissive),
        _ => None,
    }
}

/// Sortie de `getenforce`, utile quand `selinuxfs` n'est pas monté : c'est le
/// seul moyen de distinguer « désactivé » de « inexistant ».
pub fn parse_getenforce(stdout: &str) -> Option<SelinuxMode> {
    match stdout.trim().to_ascii_lowercase().as_str() {
        "enforcing" => Some(SelinuxMode::Enforcing),
        "permissive" => Some(SelinuxMode::Permissive),
        "disabled" => Some(SelinuxMode::Disabled),
        _ => None,
    }
}

/// Un noyau plus récent que celui qui tourne est-il installé ?
///
/// Repli quand le gestionnaire de paquets ne sait pas répondre : un nouveau
/// noyau attend toujours un redémarrage, quelle que soit la distribution.
pub fn newer_kernel_installed(running: &str, installed: &[String]) -> bool {
    let running = version_key(running.trim());
    installed.iter().any(|candidate| version_key(candidate.trim()) > running)
}

/// Segment de version : les nombres se comparent entre eux, et priment sur les
/// lettres, comme le fait `rpmvercmp`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Segment {
    Text(String),
    Number(u64),
}

/// Découpe `7.1.8-200.fc44.x86_64` en `[7, 1, 8, 200, fc, 44, x, 86, 64]`.
fn version_key(version: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut numeric = false;

    let flush = |current: &mut String, numeric: bool, segments: &mut Vec<Segment>| {
        if current.is_empty() {
            return;
        }
        let segment = if numeric {
            // Un nombre trop long pour un `u64` reste comparable en tant que texte.
            current.parse().map(Segment::Number).unwrap_or_else(|_| Segment::Text(current.clone()))
        } else {
            Segment::Text(current.clone())
        };
        segments.push(segment);
        current.clear();
    };

    for c in version.chars() {
        if c.is_ascii_digit() {
            if !numeric {
                flush(&mut current, numeric, &mut segments);
            }
            numeric = true;
            current.push(c);
        } else if c.is_ascii_alphabetic() {
            if numeric {
                flush(&mut current, numeric, &mut segments);
            }
            numeric = false;
            current.push(c);
        } else {
            flush(&mut current, numeric, &mut segments);
        }
    }
    flush(&mut current, numeric, &mut segments);
    segments
}

// --------------------------------------------------------------- exécution

/// Réglages du collecteur, tels que fixés par la configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemHealthConfig {
    pub enabled: bool,
    pub check_updates: bool,
    pub updates_interval: std::time::Duration,
    pub check_reboot: bool,
    pub check_failed_units: bool,
}

impl Default for SystemHealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_updates: true,
            updates_interval: std::time::Duration::from_secs(
                crate::config::DEFAULT_UPDATES_INTERVAL_SECS,
            ),
            check_reboot: true,
            check_failed_units: true,
        }
    }
}

pub use platform::SystemHealthProbe;

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;
    use std::time::Duration;

    use tokio::task::JoinHandle;
    use tokio::time::Instant;
    use tracing::debug;

    use super::{
        OsInfo, SelinuxMode, SystemHealthConfig, SystemHealthStat, UpdatesStat,
        newer_kernel_installed, parse_apt_upgradable, parse_dnf_check_update,
        parse_dnf_updateinfo_security, parse_failed_units, parse_getenforce,
        parse_needs_restarting, parse_os_release, parse_selinux_enforce,
    };

    /// `dnf` charge l'intégralité des métadonnées de dépôt avant de répondre :
    /// une à deux secondes à plein régime, dix fois plus sous quota processeur.
    const PACKAGE_MANAGER_TIMEOUT: Duration = Duration::from_secs(60);

    /// `systemctl` et `getenforce` répondent en quelques millisecondes ; au-delà
    /// de dix secondes, c'est systemd lui-même qui ne va pas bien.
    const QUICK_TIMEOUT: Duration = Duration::from_secs(10);

    /// Attente tolérée au premier cycle pour que `--dry-run` et `--once`
    /// montrent l'état des mises à jour ; `dnf` répond d'ordinaire en deux à dix
    /// secondes selon le quota processeur de l'unité.
    const FIRST_CYCLE_WAIT: Duration = Duration::from_secs(30);

    const OS_RELEASE: &str = "/etc/os-release";
    const SELINUX_ENFORCE: &str = "/sys/fs/selinux/enforce";
    const RUNNING_KERNEL: &str = "/proc/sys/kernel/osrelease";
    const KERNEL_MODULES: &str = "/lib/modules";
    /// Posé par les scripts de paquets Debian et Ubuntu (`update-notifier-common`).
    const DEBIAN_REBOOT_FLAG: &str = "/var/run/reboot-required";

    /// Gestionnaire de paquets reconnu, déduit de `/etc/os-release`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PackageManager {
        Dnf,
        Apt,
        Unknown,
    }

    impl PackageManager {
        fn detect(os: &OsInfo) -> Self {
            let family = format!("{} {}", os.id, os.id_like).to_ascii_lowercase();
            let is = |names: &[&str]| family.split_whitespace().any(|f| names.contains(&f));
            if is(&["fedora", "rhel", "centos", "rocky", "almalinux", "ol"]) {
                Self::Dnf
            } else if is(&["debian", "ubuntu"]) {
                Self::Apt
            } else {
                Self::Unknown
            }
        }
    }

    /// Résultat des vérifications lentes, celles qui passent par le gestionnaire
    /// de paquets.
    #[derive(Debug, Clone, Default)]
    struct SlowStat {
        updates: Option<UpdatesStat>,
        reboot_required: Option<bool>,
        /// Le gestionnaire de paquets attendu n'est pas installé.
        package_manager_missing: bool,
    }

    /// Lecteur de santé, conservé d'un cycle à l'autre pour porter le cache des
    /// vérifications lentes et la mémoire des outils absents.
    pub struct SystemHealthProbe {
        config: SystemHealthConfig,
        os: Option<OsInfo>,
        package_manager: PackageManager,
        slow: SlowStat,
        slow_refreshed_at: Option<Instant>,
        slow_task: Option<JoinHandle<SlowStat>>,
        /// Outils constatés absents : inutile de relancer un processus pour rien
        /// à chaque cycle, et le journal n'a besoin de l'apprendre qu'une fois.
        systemctl_missing: bool,
        getenforce_missing: bool,
    }

    impl SystemHealthProbe {
        pub fn new(config: &SystemHealthConfig) -> Self {
            let os = std::fs::read_to_string(OS_RELEASE).ok().map(|text| parse_os_release(&text));
            let package_manager =
                os.as_ref().map_or(PackageManager::Unknown, PackageManager::detect);
            Self {
                config: config.clone(),
                os,
                package_manager,
                slow: SlowStat::default(),
                slow_refreshed_at: None,
                slow_task: None,
                systemctl_missing: false,
                getenforce_missing: false,
            }
        }

        /// Un cycle de lecture. `None` quand le collecteur est désactivé.
        pub async fn read(&mut self) -> Option<SystemHealthStat> {
            if !self.config.enabled {
                return None;
            }
            self.refresh_slow_checks().await;

            let failed_units = if self.config.check_failed_units && !self.systemctl_missing {
                self.read_failed_units().await
            } else {
                None
            };
            let selinux = self.read_selinux().await;

            Some(SystemHealthStat {
                os: self.os.clone(),
                updates: self.slow.updates.clone(),
                reboot_required: self.slow.reboot_required,
                failed_units,
                selinux,
            })
        }

        /// Relance la tâche de fond quand son résultat a vieilli, et récupère
        /// celui de la tâche précédente si elle a terminé.
        ///
        /// Rien n'est attendu ici, sauf au tout premier cycle : un `dnf` qui prend
        /// dix secondes ne doit pas retarder l'envoi du reste des mesures, mais un
        /// `--dry-run` ou un `--once` d'installation doit montrer quelque chose.
        async fn refresh_slow_checks(&mut self) {
            if !self.config.check_updates && !self.config.check_reboot {
                return;
            }
            if self.slow_task.is_none() {
                let stale = self
                    .slow_refreshed_at
                    .is_none_or(|at| at.elapsed() >= self.config.updates_interval);
                if !stale {
                    return;
                }
                let config = self.config.clone();
                let package_manager = self.package_manager;
                self.slow_task = Some(tokio::spawn(slow_checks(config, package_manager)));
            }

            let Some(task) = self.slow_task.as_mut() else { return };
            let first_cycle = self.slow_refreshed_at.is_none();
            if !task.is_finished() && !first_cycle {
                return;
            }
            // Au premier cycle, on attend — mais pas indéfiniment : passé ce délai,
            // la tâche poursuit en arrière-plan et le prochain cycle la récoltera.
            let outcome = tokio::time::timeout(FIRST_CYCLE_WAIT, task).await;
            let Ok(outcome) = outcome else { return };
            self.slow_task = None;
            match outcome {
                Ok(stat) => {
                    if stat.package_manager_missing {
                        debug!("no package manager found, pending updates not reported");
                        self.package_manager = PackageManager::Unknown;
                    }
                    self.slow = stat;
                    self.slow_refreshed_at = Some(Instant::now());
                }
                Err(error) => {
                    debug!(%error, "pending-updates check aborted");
                    // On ne réessaie pas à chaque cycle : la prochaine tentative
                    // attendra la période normale.
                    self.slow_refreshed_at = Some(Instant::now());
                }
            }
        }

        async fn read_failed_units(&mut self) -> Option<Vec<String>> {
            match run("systemctl", &["--failed", "--plain", "--no-legend"], QUICK_TIMEOUT).await {
                Ok(output) if output.code == Some(0) => Some(parse_failed_units(&output.stdout)),
                // Sans systemd (conteneur, OpenRC), `systemctl` répond mais refuse.
                Ok(_) => None,
                Err(RunError::Missing) => {
                    debug!("systemctl not found, failed units not reported");
                    self.systemctl_missing = true;
                    None
                }
                Err(_) => None,
            }
        }

        async fn read_selinux(&mut self) -> Option<SelinuxMode> {
            if let Ok(text) = std::fs::read_to_string(SELINUX_ENFORCE) {
                return parse_selinux_enforce(&text);
            }
            if self.getenforce_missing {
                return None;
            }
            match run("getenforce", &[], QUICK_TIMEOUT).await {
                Ok(output) => parse_getenforce(&output.stdout),
                Err(RunError::Missing) => {
                    debug!("SELinux not present on this machine");
                    self.getenforce_missing = true;
                    None
                }
                Err(_) => None,
            }
        }
    }

    /// Les vérifications qui passent par le gestionnaire de paquets.
    async fn slow_checks(config: SystemHealthConfig, package_manager: PackageManager) -> SlowStat {
        let mut stat = SlowStat::default();
        if config.check_updates {
            match read_updates(package_manager).await {
                Ok(updates) => stat.updates = updates,
                Err(RunError::Missing) => stat.package_manager_missing = true,
                Err(RunError::Failed) => {}
            }
        }
        // Après un gestionnaire absent, seul le repli sur le noyau a un sens.
        let package_manager =
            if stat.package_manager_missing { PackageManager::Unknown } else { package_manager };
        if config.check_reboot {
            stat.reboot_required = read_reboot_required(package_manager).await;
        }
        stat
    }

    /// `Ok(None)` : gestionnaire présent mais réponse inexploitable (cache
    /// absent, dépôt cassé). `Err(Missing)` : gestionnaire absent.
    async fn read_updates(
        package_manager: PackageManager,
    ) -> Result<Option<UpdatesStat>, RunError> {
        match package_manager {
            PackageManager::Dnf => {
                // `--cacheonly` : jamais de réseau depuis l'agent. Les métadonnées
                // sont celles que `dnf-makecache.timer` entretient.
                let output =
                    run("dnf", &["-q", "--cacheonly", "check-update"], PACKAGE_MANAGER_TIMEOUT)
                        .await?;
                let Some(pending) = parse_dnf_check_update(&output.stdout, output.code) else {
                    return Ok(None);
                };
                let security = run(
                    "dnf",
                    &["-q", "--cacheonly", "updateinfo", "list", "--security"],
                    PACKAGE_MANAGER_TIMEOUT,
                )
                .await
                .ok()
                .filter(|output| output.code == Some(0))
                .map(|output| parse_dnf_updateinfo_security(&output.stdout));
                Ok(Some(UpdatesStat { pending, security }))
            }
            PackageManager::Apt => {
                let output = run("apt", &["list", "--upgradable"], PACKAGE_MANAGER_TIMEOUT).await?;
                Ok((output.code == Some(0)).then(|| parse_apt_upgradable(&output.stdout)))
            }
            PackageManager::Unknown => Ok(None),
        }
    }

    async fn read_reboot_required(package_manager: PackageManager) -> Option<bool> {
        match package_manager {
            PackageManager::Dnf => {
                let verdict = run(
                    "dnf",
                    &["-q", "--cacheonly", "needs-restarting", "-r"],
                    PACKAGE_MANAGER_TIMEOUT,
                )
                .await
                .ok()
                .and_then(|output| parse_needs_restarting(&output.stdout, output.code));
                verdict.or_else(newer_kernel_than_running)
            }
            PackageManager::Apt => {
                if Path::new(DEBIAN_REBOOT_FLAG).exists() {
                    return Some(true);
                }
                // Le drapeau n'est posé que si `update-notifier-common` est
                // installé : son absence ne prouve rien, le noyau tranche.
                Some(newer_kernel_than_running().unwrap_or(false))
            }
            PackageManager::Unknown => newer_kernel_than_running(),
        }
    }

    /// Compare le noyau qui tourne aux noyaux installés dans `/lib/modules`.
    ///
    /// Seuls les répertoires portant un sous-répertoire `kernel/` comptent : un
    /// noyau désinstallé laisse parfois derrière lui un répertoire vidé de tout
    /// sauf de modules tiers, qui ne se démarre pas.
    fn newer_kernel_than_running() -> Option<bool> {
        let running = std::fs::read_to_string(RUNNING_KERNEL).ok()?;
        let installed: Vec<String> = std::fs::read_dir(KERNEL_MODULES)
            .ok()?
            .flatten()
            .filter(|entry| entry.path().join("kernel").is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        if installed.is_empty() {
            return None;
        }
        Some(newer_kernel_installed(&running, &installed))
    }

    struct Output {
        code: Option<i32>,
        stdout: String,
    }

    enum RunError {
        /// Le programme n'est pas installé.
        Missing,
        /// Impossible à lancer, ou trop long.
        Failed,
    }

    /// Lance un programme sans interpréteur, avec un délai strict.
    async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<Output, RunError> {
        let command = tokio::process::Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();

        match tokio::time::timeout(timeout, command).await {
            Ok(Ok(output)) => Ok(Output {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            }),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(RunError::Missing)
            }
            Ok(Err(error)) => {
                debug!(program, %error, "command could not be started");
                Err(RunError::Failed)
            }
            Err(_) => {
                debug!(program, timeout_s = timeout.as_secs(), "command aborted, took too long");
                Err(RunError::Failed)
            }
        }
    }
}

/// Hors Linux, le collecteur existe mais ne mesure rien : Windows a son propre
/// gestionnaire de mises à jour, sans équivalent lisible depuis un binaire.
#[cfg(not(target_os = "linux"))]
mod platform {
    use super::{SystemHealthConfig, SystemHealthStat};

    pub struct SystemHealthProbe;

    impl SystemHealthProbe {
        pub fn new(_config: &SystemHealthConfig) -> Self {
            Self
        }

        pub async fn read(&mut self) -> Option<SystemHealthStat> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(samples: &[Sample], series_key: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == series_key).map(|s| s.value)
    }

    // Sorties réelles, relevées sur un Fedora Server 44 le 4 septembre 2026.

    const OS_RELEASE_FEDORA: &str = r#"NAME="Fedora Linux"
VERSION="44 (Server Edition)"
RELEASE_TYPE=stable
ID=fedora
VERSION_ID=44
VERSION_CODENAME=""
PRETTY_NAME="Fedora Linux 44 (Server Edition)"
ANSI_COLOR="0;38;2;60;110;180"
LOGO=fedora-logo-icon
CPE_NAME="cpe:/o:fedoraproject:fedora:44"
HOME_URL="https://fedoraproject.org/"
SUPPORT_END=2027-05-19
VARIANT="Server Edition"
VARIANT_ID=server
"#;

    const OS_RELEASE_DEBIAN: &str = r#"PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"
NAME="Debian GNU/Linux"
VERSION_ID="12"
VERSION="12 (bookworm)"
VERSION_CODENAME=bookworm
ID=debian
HOME_URL="https://www.debian.org/"
"#;

    const OS_RELEASE_UBUNTU: &str = r#"PRETTY_NAME="Ubuntu 24.04.1 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
VERSION="24.04.1 LTS (Noble Numbat)"
VERSION_CODENAME=noble
ID=ubuntu
ID_LIKE=debian
UBUNTU_CODENAME=noble
"#;

    const OS_RELEASE_ALMA: &str = r#"NAME="AlmaLinux"
VERSION="9.4 (Seafoam Ocelot)"
ID="almalinux"
ID_LIKE="rhel centos fedora"
VERSION_ID="9.4"
PLATFORM_ID="platform:el9"
PRETTY_NAME="AlmaLinux 9.4 (Seafoam Ocelot)"
"#;

    const DNF5_CHECK_UPDATE: &str = "Upgrades
btrfs-progs.x86_64           7.1-1.fc44      updates
docker-compose-plugin.x86_64 5.5.0-1.fc44    docker-ce-stable
exfatprogs.x86_64            1.4.3-1.fc44    updates
grub2-common.noarch          1:2.12-64.fc44  updates
grub2-efi-x64.x86_64         1:2.12-64.fc44  updates
kernel.x86_64                7.1.13-200.fc44 updates
python3-boto3.noarch         1.43.72-1.fc44  updates
smartmontools-selinux.noarch 1:7.5-9.fc44    updates
";

    const DNF4_CHECK_UPDATE: &str = "
kernel.x86_64                        6.5.5-200.fc38                 updates
kernel-core.x86_64                   6.5.5-200.fc38                 updates
python3-setuptools-wheel.noarch
                                     59.6.0-4.fc38                  updates
Obsoleting Packages
grub2-tools.x86_64                   1:2.06-100.fc38                updates
    grub2-tools.x86_64               1:2.06-99.fc38                 @updates
";

    const DNF5_UPDATEINFO_SECURITY: &str = "Name                   Type     Severity                                    Package              Issued
FEDORA-2026-0d885c0533 security Moderate              kernel-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate         kernel-core-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate      kernel-modules-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate kernel-modules-core-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate        kernel-tools-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate   kernel-tools-libs-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
FEDORA-2026-0d885c0533 security Moderate        python3-perf-7.1.13-200.fc44.x86_64 2026-09-03 01:37:20
";

    const DNF4_UPDATEINFO_SECURITY: &str = "\
FEDORA-2023-8f4a2c1d3e Important/Sec. kernel-6.5.5-200.fc38.x86_64
FEDORA-2023-8f4a2c1d3e Important/Sec. kernel-core-6.5.5-200.fc38.x86_64
FEDORA-2023-1b2c3d4e5f Moderate/Sec.  kernel-core-6.5.5-200.fc38.x86_64
FEDORA-2023-a1b2c3d4e5 bugfix         vim-minimal-9.0.2000-1.fc38.x86_64
";

    const APT_UPGRADABLE: &str = "Listing...
base-files/bookworm 12.4+deb12u7 amd64 [upgradable from: 12.4+deb12u5]
libssl3/bookworm-security 3.0.13-1~deb12u1 amd64 [upgradable from: 3.0.11-1~deb12u2]
openssl/bookworm-security,bookworm-updates 3.0.13-1~deb12u1 amd64 [upgradable from: 3.0.11-1~deb12u2]
";

    const SYSTEMCTL_FAILED: &str =
        "mdmonitor.service loaded failed failed Software RAID monitoring and management\n";

    #[test]
    fn fedora_os_release_is_parsed() {
        let os = parse_os_release(OS_RELEASE_FEDORA);
        assert_eq!(
            os,
            OsInfo {
                id: "fedora".into(),
                name: "Fedora Linux".into(),
                version_id: "44".into(),
                pretty_name: "Fedora Linux 44 (Server Edition)".into(),
                id_like: String::new(),
            }
        );
    }

    #[test]
    fn debian_ubuntu_and_alma_os_releases_are_parsed() {
        let debian = parse_os_release(OS_RELEASE_DEBIAN);
        assert_eq!(debian.id, "debian");
        assert_eq!(debian.version_id, "12");
        assert_eq!(debian.pretty_name, "Debian GNU/Linux 12 (bookworm)");

        let ubuntu = parse_os_release(OS_RELEASE_UBUNTU);
        assert_eq!(ubuntu.id, "ubuntu");
        assert_eq!(ubuntu.id_like, "debian");
        assert_eq!(ubuntu.version_id, "24.04");

        let alma = parse_os_release(OS_RELEASE_ALMA);
        assert_eq!(alma.id, "almalinux");
        assert_eq!(alma.id_like, "rhel centos fedora");
        assert_eq!(alma.name, "AlmaLinux");
    }

    #[test]
    fn os_release_escapes_and_comments_are_handled() {
        let os = parse_os_release("# commentaire\nNAME='Ma \\\"distro\\\"'\n\nID = perso\n");
        assert_eq!(os.name, "Ma \"distro\"");
        assert_eq!(os.id, "perso");
    }

    #[test]
    fn dnf5_pending_updates_are_counted_per_package() {
        assert_eq!(parse_dnf_check_update(DNF5_CHECK_UPDATE, Some(100)), Some(8));
    }

    #[test]
    fn dnf4_wrapped_lines_and_obsoletes_are_not_double_counted() {
        // Le nom replié sur deux lignes compte une fois ; la section des paquets
        // remplacés ne compte pas du tout.
        assert_eq!(parse_dnf_check_update(DNF4_CHECK_UPDATE, Some(100)), Some(3));
    }

    #[test]
    fn dnf_exit_codes_decide_between_zero_and_unknown() {
        assert_eq!(parse_dnf_check_update("", Some(0)), Some(0));
        // Cache absent, dépôt cassé : ce n'est pas « zéro mise à jour ».
        assert_eq!(parse_dnf_check_update("", Some(1)), None);
        assert_eq!(parse_dnf_check_update("", None), None);
        // Un message informatif n'est pas un paquet.
        assert_eq!(
            parse_dnf_check_update(
                "No security updates needed, but 66 update(s) available\n",
                Some(100)
            ),
            Some(0)
        );
    }

    #[test]
    fn dnf5_security_advisories_are_counted_per_package() {
        assert_eq!(parse_dnf_updateinfo_security(DNF5_UPDATEINFO_SECURITY), 7);
        assert_eq!(parse_dnf_updateinfo_security(""), 0);
    }

    #[test]
    fn dnf4_security_advisories_are_deduplicated_by_package() {
        // `kernel-core` relève de deux avis, et la ligne « bugfix » ne compte pas.
        assert_eq!(parse_dnf_updateinfo_security(DNF4_UPDATEINFO_SECURITY), 2);
    }

    #[test]
    fn apt_upgradable_lines_are_counted_with_their_security_suite() {
        assert_eq!(
            parse_apt_upgradable(APT_UPGRADABLE),
            UpdatesStat { pending: 3, security: Some(2) }
        );
        assert_eq!(
            parse_apt_upgradable("Listing...\n"),
            UpdatesStat { pending: 0, security: Some(0) }
        );
    }

    #[test]
    fn needs_restarting_is_read_from_the_text_first() {
        let no = "No core libraries or services have been updated since boot-up.\nReboot should not be necessary.\n";
        assert_eq!(parse_needs_restarting(no, Some(0)), Some(false));

        let yes = "Core libraries or services have been updated since boot-up:\n  * kernel\n\nReboot is required to fully utilize these updates.\n";
        assert_eq!(parse_needs_restarting(yes, Some(1)), Some(true));

        let dnf4 = "Reboot is required to ensure that your systems are up to date.\nMore information: https://access.redhat.com/solutions/27943\n";
        assert_eq!(parse_needs_restarting(dnf4, Some(1)), Some(true));

        // Code 1 sans phrase reconnue : dnf 4 renvoie aussi 1 pour « commande
        // inconnue », on ne conclut pas.
        assert_eq!(parse_needs_restarting("No such command: needs-restarting\n", Some(1)), None);
        assert_eq!(parse_needs_restarting("", Some(0)), Some(false));
    }

    #[test]
    fn failed_units_are_read_from_the_first_column() {
        assert_eq!(parse_failed_units(SYSTEMCTL_FAILED), vec!["mdmonitor.service"]);
        assert!(parse_failed_units("").is_empty());
        assert_eq!(
            parse_failed_units("● nginx.service loaded failed failed nginx\n"),
            vec!["nginx.service"]
        );
    }

    #[test]
    fn selinux_modes_are_read_from_sysfs_or_getenforce() {
        assert_eq!(parse_selinux_enforce("1\n"), Some(SelinuxMode::Enforcing));
        assert_eq!(parse_selinux_enforce("0"), Some(SelinuxMode::Permissive));
        assert_eq!(parse_selinux_enforce("garbage"), None);
        assert_eq!(parse_getenforce("Enforcing\n"), Some(SelinuxMode::Enforcing));
        assert_eq!(parse_getenforce("Permissive\n"), Some(SelinuxMode::Permissive));
        assert_eq!(parse_getenforce("Disabled\n"), Some(SelinuxMode::Disabled));
    }

    #[test]
    fn kernel_versions_are_compared_numerically() {
        let installed = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 13 > 8, ce qu'un tri lexicographique aurait nié.
        assert!(newer_kernel_installed(
            "7.1.8-200.fc44.x86_64",
            &installed(&["7.1.8-200.fc44.x86_64", "7.1.13-200.fc44.x86_64"])
        ));
        assert!(!newer_kernel_installed(
            "7.1.13-200.fc44.x86_64",
            &installed(&["7.1.8-200.fc44.x86_64", "7.1.13-200.fc44.x86_64"])
        ));
        assert!(newer_kernel_installed("6.1.0-18-amd64", &installed(&["6.1.0-21-amd64"])));
        assert!(!newer_kernel_installed("6.1.0-21-amd64", &installed(&["6.1.0-21-amd64"])));
        assert!(!newer_kernel_installed("6.1.0-21-amd64", &[]));
    }

    #[test]
    fn every_family_becomes_a_sample() {
        let stat = SystemHealthStat {
            os: Some(parse_os_release(OS_RELEASE_UBUNTU)),
            updates: Some(UpdatesStat { pending: 12, security: Some(3) }),
            reboot_required: Some(true),
            failed_units: Some(vec!["mdmonitor.service".into()]),
            selinux: Some(SelinuxMode::Enforcing),
        };
        let samples = samples(&stat, 1_000);

        assert!(samples.iter().all(|s| s.ts_ms == 1_000 && s.kind == MetricKind::Gauge));
        assert_eq!(
            value_of(
                &samples,
                r#"agent_os_info{id="ubuntu",id_like="debian",name="Ubuntu",pretty_name="Ubuntu 24.04.1 LTS",version_id="24.04"}"#
            ),
            Some(1.0)
        );
        assert_eq!(value_of(&samples, "agent_updates_pending"), Some(12.0));
        assert_eq!(value_of(&samples, "agent_security_updates_pending"), Some(3.0));
        assert_eq!(value_of(&samples, "agent_reboot_required"), Some(1.0));
        assert_eq!(value_of(&samples, "agent_systemd_failed_units"), Some(1.0));
        assert_eq!(
            value_of(&samples, r#"agent_systemd_unit_failed{unit="mdmonitor.service"}"#),
            Some(1.0)
        );
        assert_eq!(value_of(&samples, "agent_selinux_mode"), Some(2.0));
    }

    #[test]
    fn an_unknown_value_produces_no_series_at_all() {
        // Le principe du module : pas de zéro mensonger.
        assert!(samples(&SystemHealthStat::default(), 0).is_empty());

        let no_security = SystemHealthStat {
            updates: Some(UpdatesStat { pending: 4, security: None }),
            ..SystemHealthStat::default()
        };
        let produced = samples(&no_security, 0);
        assert_eq!(value_of(&produced, "agent_updates_pending"), Some(4.0));
        assert!(!produced.iter().any(|s| s.metric == "agent_security_updates_pending"));
    }

    #[test]
    fn empty_os_release_labels_are_omitted() {
        let stat = SystemHealthStat {
            os: Some(parse_os_release(OS_RELEASE_FEDORA)),
            ..SystemHealthStat::default()
        };
        let sample = &samples(&stat, 0)[0];
        assert!(!sample.labels.contains_key("id_like"));
        assert_eq!(sample.labels.get("version_id").map(String::as_str), Some("44"));
    }

    #[test]
    fn failed_unit_series_are_capped_but_the_count_is_not() {
        let units: Vec<String> = (0..80).map(|i| format!("unite-{i}.service")).collect();
        let stat = SystemHealthStat { failed_units: Some(units), ..SystemHealthStat::default() };
        let samples = samples(&stat, 0);
        assert_eq!(value_of(&samples, "agent_systemd_failed_units"), Some(80.0));
        assert_eq!(
            samples.iter().filter(|s| s.metric == "agent_systemd_unit_failed").count(),
            MAX_FAILED_UNIT_SERIES
        );
    }

    #[tokio::test]
    async fn a_disabled_collector_reads_nothing() {
        let config = SystemHealthConfig { enabled: false, ..SystemHealthConfig::default() };
        let mut probe = SystemHealthProbe::new(&config);
        assert!(probe.read().await.is_none());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_cycle_on_this_machine_completes_without_blocking() {
        // Sans dnf ni systemd (conteneur de test), chaque famille est simplement
        // absente ; ce qui compte est que rien ne bloque et que rien n'explose.
        let mut probe = SystemHealthProbe::new(&SystemHealthConfig::default());
        let stat = tokio::time::timeout(std::time::Duration::from_secs(15), probe.read())
            .await
            .expect("un cycle ne doit pas attendre les vérifications lentes")
            .expect("collecteur actif");
        assert!(samples(&stat, 0).iter().all(|s| s.value.is_finite()));
    }
}
