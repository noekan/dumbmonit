//! Structures de désérialisation des réponses de l'API web de DSM.
//!
//! Deux règles gouvernent ce module, apprises de l'API Synology :
//!
//! * **tous les champs métier sont optionnels.** DSM n'expose pas les mêmes clés
//!   selon le modèle de NAS, la version de DSM et le type de volume : un NAS sans
//!   sonde de température n'a pas de `sys_temp`, un volume Btrfs n'a pas les mêmes
//!   champs qu'un volume ext4. Une clé absente doit produire une métrique en
//!   moins, jamais une erreur ;
//! * **les nombres arrivent parfois en chaînes.** Les tailles de volume sont des
//!   chaînes d'octets (`"5343156371456"`) parce qu'elles dépassent la précision
//!   d'un entier JavaScript, et certains champs alternent selon la version. D'où
//!   [`Num`], qui accepte les deux formes.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Enveloppe commune à toutes les réponses : `{"success": …, "data": …}`.
///
/// `error` n'est présent qu'en cas d'échec, et l'échec arrive en HTTP 200 : c'est
/// donc `success` qui fait foi.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    #[serde(default)]
    pub success: bool,
    // Pas de `#[serde(default)]` sur ces deux champs : serde traite déjà un
    // `Option` absent comme `None`, et l'attribut imposerait à `T` une borne
    // `Default` que les types métier n'ont aucune raison de porter.
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

impl<T> Envelope<T> {
    /// Code d'erreur applicatif, ou `None` si la requête a abouti.
    ///
    /// Une réponse en échec sans objet `error` est rapportée au code 100
    /// (« erreur inconnue ») plutôt qu'ignorée : mieux vaut une erreur floue
    /// qu'une absence de métriques silencieuse.
    pub fn error_code(&self) -> Option<i64> {
        if self.success {
            return None;
        }
        Some(self.error.as_ref().map_or(100, |error| error.code))
    }
}

#[derive(Debug, Deserialize)]
pub struct ApiError {
    #[serde(default = "unknown_error_code")]
    pub code: i64,
}

fn unknown_error_code() -> i64 {
    100
}

/// Réponse de `SYNO.API.Auth&method=login`.
///
/// Ne dérive pas `Debug` : `sid` et `synotoken` valent mot de passe.
#[derive(Deserialize)]
pub struct LoginData {
    pub sid: String,
    #[serde(default)]
    pub synotoken: Option<String>,
}

impl fmt::Debug for LoginData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoginData { sid: <redacted>, synotoken: <redacted> }")
    }
}

/// Réponse de `SYNO.API.Info&method=query` : un objet dont les clés sont les noms
/// d'API.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct ApiCatalog(BTreeMap<String, ApiDescriptor>);

#[derive(Debug, Clone, Deserialize)]
pub struct ApiDescriptor {
    pub path: String,
    #[serde(rename = "minVersion", default = "first_version")]
    pub min_version: u32,
    #[serde(rename = "maxVersion", default = "first_version")]
    pub max_version: u32,
}

fn first_version() -> u32 {
    1
}

/// Chemin et version retenus pour un appel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub path: String,
    pub version: u32,
}

impl ApiCatalog {
    pub fn contains(&self, api: &str) -> bool {
        self.resolve(api, 1).is_some()
    }

    /// Choisit le chemin et la version à utiliser pour une API.
    ///
    /// La version souhaitée est ramenée dans l'intervalle annoncé par le NAS : on
    /// obtient ainsi la version la plus proche de celle qu'on sait exploiter, sans
    /// jamais demander une version que ce DSM ne connaît pas — c'est ce qui rend le
    /// collecteur utilisable de DSM 6 à DSM 7 sans branchement par version.
    pub fn resolve(&self, api: &str, desired: u32) -> Option<Endpoint> {
        let descriptor = self.0.get(api)?;
        let min = descriptor.min_version.max(1);
        // Un NAS qui annoncerait un intervalle incohérent ne doit pas produire une
        // version nulle : on préfère retomber sur la borne basse.
        let max = descriptor.max_version.max(min);
        Some(Endpoint { path: safe_path(&descriptor.path)?, version: desired.clamp(min, max) })
    }
}

/// Valide le chemin annoncé par le NAS avant de le coller derrière `/webapi/`.
///
/// Le chemin vient de la réponse d'un équipement du réseau : le concaténer sans
/// contrôle laisserait un NAS compromis — ou un serveur qui se fait passer pour
/// lui — diriger nos requêtes authentifiées ailleurs, par un `../` ou une URL
/// absolue. On n'accepte donc qu'un nom de fichier CGI éventuellement précédé d'un
/// dossier, ce qui couvre `entry.cgi` comme `VideoStation/info.cgi`.
fn safe_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.starts_with('/') || path.contains("..") {
        return None;
    }
    let acceptable =
        path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    acceptable.then(|| path.to_string())
}

/// `SYNO.Core.System&method=info` : identité et état général du NAS.
///
/// Les champs varient beaucoup d'un modèle à l'autre : un NAS d'entrée de gamme
/// n'a pas de sonde `sys_temp`, et `temperature_warn` n'apparaît que sur les
/// modèles qui savent signaler une surchauffe.
#[derive(Debug, Default, Deserialize)]
pub struct SystemInfo {
    #[serde(default)]
    pub model: Option<String>,
    /// Version de DSM, sous la forme « DSM 7.2.1-69057 Update 5 ».
    #[serde(default)]
    pub firmware_ver: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    /// Durée de fonctionnement, en texte : voir [`crate::synology::metrics`].
    #[serde(default)]
    pub up_time: Option<String>,
    /// Heure locale du NAS, au format de `ctime` : « Sun Aug 31 14:22:03 2025 ».
    /// Seule référence permettant de dater les sauvegardes sans connaître le fuseau.
    #[serde(default)]
    pub time: Option<String>,
    /// Température du boîtier, en degrés Celsius.
    #[serde(default)]
    pub sys_temp: Option<Num>,
    /// Drapeau de surchauffe levé par DSM.
    ///
    /// DSM 7.4 nomme ce champ `temperature_warning` ; les DSM plus anciens
    /// `temperature_warn`. Relevé sur un DS918+ en DSM 7.4.1 : la réponse porte
    /// `temperature_warning`, `sys_tempwarn` et `systempwarn`, mais jamais
    /// `temperature_warn` — sans cet alias, le drapeau n'était jamais lu. Un seul
    /// alias est déclaré : serde refuse une réponse où deux noms d'un même champ
    /// coexisteraient.
    #[serde(default, alias = "temperature_warning")]
    pub temperature_warn: Option<Num>,
    /// Mémoire installée, en mébioctets.
    #[serde(default)]
    pub ram_size: Option<Num>,
    #[serde(default)]
    pub cpu_family: Option<String>,
    /// Nombre de cœurs. DSM le renvoie en chaîne, d'où [`Num`].
    #[serde(default)]
    pub cpu_cores: Option<Num>,
    /// Fréquence du processeur, en mégahertz.
    #[serde(default)]
    pub cpu_clock_speed: Option<Num>,
}

/// `SYNO.Core.System.Utilization&method=get` : instantané de la charge.
#[derive(Debug, Default, Deserialize)]
pub struct Utilization {
    #[serde(default)]
    pub cpu: Option<CpuUsage>,
    #[serde(default)]
    pub memory: Option<MemoryUsage>,
    #[serde(default)]
    pub network: Vec<NetworkUsage>,
}

/// Les trois charges se cumulent pour donner l'occupation totale du processeur.
/// Elles sont déjà exprimées en pourcentage.
#[derive(Debug, Default, Deserialize)]
pub struct CpuUsage {
    #[serde(default)]
    pub user_load: Option<Num>,
    #[serde(default)]
    pub system_load: Option<Num>,
    #[serde(default)]
    pub other_load: Option<Num>,
    // `1min_load`, `5min_load` et `15min_load` sont volontairement absents : leur
    // échelle n'est pas documentée et les relevés la contredisent. Voir `metrics.rs`.
}

/// Toutes les tailles de ce bloc sont en **kibioctets**.
///
/// L'unité change d'une API à l'autre sur le même NAS : `SYNO.Core.System` donne
/// `ram_size` en mébioctets et le bloc `env` du stockage en gibioctets. Confondre
/// les trois est l'erreur qui produit des graphes de mémoire faux d'un facteur mille.
#[derive(Debug, Default, Deserialize)]
pub struct MemoryUsage {
    #[serde(default)]
    pub total_real: Option<Num>,
    #[serde(default)]
    pub avail_real: Option<Num>,
    /// Occupation en pourcentage, telle que DSM l'affiche.
    #[serde(default)]
    pub real_usage: Option<Num>,
    #[serde(default)]
    pub cached: Option<Num>,
    #[serde(default)]
    pub buffer: Option<Num>,
    #[serde(default)]
    pub total_swap: Option<Num>,
    #[serde(default)]
    pub avail_swap: Option<Num>,
    #[serde(default)]
    pub swap_usage: Option<Num>,
}

/// Débit instantané d'une interface, en octets par seconde. L'entrée `total`
/// agrège toutes les interfaces.
#[derive(Debug, Default, Deserialize)]
pub struct NetworkUsage {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub rx: Option<Num>,
    #[serde(default)]
    pub tx: Option<Num>,
}

/// `SYNO.Storage.CGI.Storage&method=load_info`.
#[derive(Debug, Default, Deserialize)]
pub struct StorageInfo {
    #[serde(default)]
    pub volumes: Vec<Volume>,
    #[serde(default)]
    pub disks: Vec<Disk>,
    /// Groupes de stockage (RAID). Déjà présents dans la réponse que l'on demande
    /// pour les volumes : un groupe dégradé se voit ici avant que le volume posé
    /// dessus ne change d'état.
    #[serde(rename = "storagePools", default)]
    pub storage_pools: Vec<Pool>,
    /// Caches SSD. Même réponse, même raison : un cache en lecture-écriture
    /// défaillant met les données en danger sans toucher à l'état du volume.
    #[serde(rename = "ssdCaches", default)]
    pub ssd_caches: Vec<Pool>,
    #[serde(default)]
    pub env: Option<StorageEnv>,
}

/// Un groupe de stockage ou un cache SSD : DSM leur donne la même forme dans
/// `load_info`, au point que les distinguer par un type n'apporterait rien.
#[derive(Debug, Default, Deserialize)]
pub struct Pool {
    pub id: String,
    #[serde(default)]
    pub desc: Option<String>,
    /// `normal`, `background`, `degrade`, `crashed`…
    #[serde(default)]
    pub status: Option<String>,
    /// Type d'assemblage : `raid_5`, `shr`, `basic`…
    #[serde(default)]
    pub device_type: Option<String>,
    /// Nombre de disques en panne dans le groupe.
    #[serde(default)]
    pub disk_failure_number: Option<Num>,
    #[serde(default)]
    pub size: Option<VolumeSize>,
}

/// Bloc `env` de `load_info` : le contexte que le NAS applique à son stockage.
#[derive(Debug, Default, Deserialize)]
pub struct StorageEnv {
    /// Fraction d'espace **libre** en deçà de laquelle DSM avertit : `0.2` signifie
    /// « prévenir à 80 % d'occupation ».
    #[serde(default)]
    pub volume_full_warning: Option<Num>,
    /// Même chose pour le seuil critique : `0.1` vaut 90 % d'occupation.
    #[serde(default)]
    pub volume_full_critical: Option<Num>,
    #[serde(default)]
    pub status: Option<StorageEnvStatus>,
}

#[derive(Debug, Default, Deserialize)]
pub struct StorageEnvStatus {
    #[serde(default)]
    pub system_crashed: Option<Num>,
    #[serde(default)]
    pub system_need_repair: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Volume {
    pub id: String,
    /// Description saisie par l'utilisateur dans DSM. Souvent vide : ce n'est pas
    /// un nom d'affichage garanti, d'où le repli sur `id`.
    ///
    /// DSM 7 la nomme `vol_desc` dans `volumes` (relevé sur DSM 7.4.1) et `desc`
    /// dans `storagePools` ; les DSM plus anciens s'en tenaient à `desc`.
    #[serde(default, alias = "vol_desc")]
    pub desc: Option<String>,
    /// Point de montage : `/volume1`. C'est lui que l'utilisateur reconnaît quand
    /// la description est vide.
    #[serde(default)]
    pub vol_path: Option<String>,
    /// `normal`, `background`, `degrade`, `crashed`…
    #[serde(default)]
    pub status: Option<String>,
    /// `ext4`, `btrfs`.
    #[serde(default)]
    pub fs_type: Option<String>,
    /// Type d'assemblage : `raid_5`, `shr`, `raid_1`, `single`…
    #[serde(default)]
    pub device_type: Option<String>,
    #[serde(default)]
    pub size: Option<VolumeSize>,
}

/// Tailles en octets, renvoyées en chaînes : elles dépassent la précision entière
/// du JSON sur un volume de plusieurs téraoctets.
#[derive(Debug, Default, Deserialize)]
pub struct VolumeSize {
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Disk {
    pub id: String,
    /// Libellé de la baie, tel qu'affiché par DSM : « Disque 1 ».
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    /// Version du micrologiciel du disque.
    #[serde(default)]
    pub firm: Option<String>,
    /// Température, en degrés Celsius.
    #[serde(default)]
    pub temp: Option<Num>,
    /// `normal`, `crashed`, `init`…
    #[serde(default)]
    pub status: Option<String>,
    /// État S.M.A.R.T. : `normal`, `warning`, `critical`…
    #[serde(default)]
    pub smart_status: Option<String>,
    #[serde(rename = "diskType", default)]
    pub disk_type: Option<String>,
    /// Capacité en octets, renvoyée en chaîne.
    #[serde(default)]
    pub size_total: Option<Num>,
    /// Seuil de secteurs réalloués dépassé : c'est le signal de préfaillance le
    /// plus direct que DSM expose — jusqu'à DSM 7.1. **DSM 7.4 ne renvoie plus ce
    /// champ** : voir [`Disk::sb_days_left_critical`].
    #[serde(default)]
    pub exceed_bad_sector_thr: Option<Num>,
    /// Verdict de DSM 7.4 sur les secteurs défectueux : il estime le nombre de
    /// jours avant que le disque n'atteigne le seuil, et lève ce drapeau quand il
    /// n'en reste plus. C'est le successeur d'`exceed_bad_sector_thr`, et sans lui
    /// la règle « secteurs défectueux » ne se déclencherait plus jamais sur un NAS
    /// à jour.
    #[serde(default)]
    pub sb_days_left_critical: Option<Num>,
    /// Durée de vie résiduelle sous le seuil, pour les SSD.
    #[serde(default)]
    pub below_remain_life_thr: Option<Num>,
    /// Même question, posée par DSM 7.4 : la durée de vie résiduelle est entrée
    /// dans la zone rouge.
    #[serde(default)]
    pub remain_life_danger: Option<Num>,
    /// Durée de vie résiduelle d'un SSD, en pourcentage ; `-1` quand le disque
    /// n'en déclare pas (tous les disques mécaniques).
    #[serde(default)]
    pub remain_life: Option<RemainLife>,
    /// Compteur `unc` de DSM : les secteurs illisibles (UNC) relevés sur le disque,
    /// celui que DSM confronte à son seuil de secteurs défectueux.
    #[serde(default)]
    pub unc: Option<Num>,
    #[serde(rename = "isSsd", default)]
    pub is_ssd: Option<Num>,
}

/// Durée de vie résiduelle d'un disque, telle que `load_info` la renvoie.
///
/// Deux formes existent, et c'est la seconde qui a fait tomber l'inventaire du
/// stockage entier sur un vrai NAS : un DSM ancien renvoie un simple pourcentage
/// (`"remain_life": -1`), DSM 7.2 et suivants un objet
/// (`"remain_life": {"value": 97, "trustable": true}`, relevé sur DS918+ en
/// DSM 7.4.1). Comme [`Num`] refuse un objet, la lecture de `load_info` échouait
/// d'un bloc : plus aucun volume, aucun disque, aucune température.
///
/// L'ordre des variantes compte : un objet ne peut pas se lire en [`Num`], donc
/// serde retombe sur la forme détaillée.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum RemainLife {
    /// Forme ancienne : le pourcentage seul.
    Plain(Num),
    /// Forme DSM 7.2+ : la valeur et la confiance que DSM lui accorde.
    Detailed {
        #[serde(default)]
        value: Option<Num>,
        #[serde(default)]
        trustable: Option<Num>,
    },
}

impl RemainLife {
    /// Pourcentage exploitable, ou `None`.
    ///
    /// `None` couvre les trois cas où publier un point serait mentir : le disque
    /// ne déclare rien (`-1`, tous les disques mécaniques), DSM juge la valeur non
    /// fiable (`trustable: false`), ou la valeur sort de l'intervalle 0–100.
    pub fn percent(self) -> Option<f64> {
        let (value, trustable) = match self {
            Self::Plain(value) => (Some(value), None),
            Self::Detailed { value, trustable } => (value, trustable),
        };
        if trustable.is_some_and(|flag| flag.0 == 0.0) {
            return None;
        }
        value.map(|n| n.0).filter(|percent| (0.0..=100.0).contains(percent))
    }
}

/// `SYNO.Backup.Task&method=list` : l'inventaire des tâches Hyper Backup.
///
/// Sans paramètre `additional`, cette réponse ne contient **aucune date** : c'est
/// pourquoi l'état de chaque tâche demande un second appel.
#[derive(Debug, Default, Deserialize)]
pub struct BackupTaskList {
    #[serde(default)]
    pub task_list: Vec<BackupTask>,
}

#[derive(Debug, Default, Deserialize)]
pub struct BackupTask {
    pub task_id: Num,
    #[serde(default)]
    pub name: Option<String>,
    /// Nature de la destination : `image`, `folder`…
    #[serde(default)]
    pub target_type: Option<String>,
}

/// `SYNO.Backup.Task&method=status` : dates et résultat de la dernière exécution.
///
/// Toutes les dates sont des chaînes à l'heure locale du NAS ; une chaîne vide
/// signifie « jamais exécutée ».
#[derive(Debug, Default, Deserialize)]
pub struct BackupStatus {
    #[serde(default)]
    pub last_bkp_time: Option<String>,
    #[serde(default)]
    pub last_bkp_success_time: Option<String>,
    /// `done`, `failed`, `dest_missing`, `backingup`, `none`…
    #[serde(default)]
    pub last_bkp_result: Option<String>,
    #[serde(default)]
    pub next_bkp_time: Option<String>,
}

/// `SYNO.ActiveBackup.Task&method=list` : les tâches Active Backup for Business.
///
/// L'API n'est pas documentée par Synology. La forme retenue ici est celle relevée
/// par le projet `N4S4/synology-api` (`core_active_backup.py`, exemple de réponse
/// obtenu avec `load_status`, `load_result` et `load_devices` à vrai) : voir
/// [`crate::synology::abb`] pour les sources. Tout est optionnel,
/// sauf `task_id` — sans lui, rien ne rattache une mesure à une tâche.
#[derive(Debug, Default, Deserialize)]
pub struct AbbTaskList {
    #[serde(default)]
    pub tasks: Vec<AbbTask>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbbTask {
    pub task_id: Num,
    #[serde(default)]
    pub task_name: Option<String>,
    /// Nature de la source : 1 machine virtuelle, 2 PC, 3 serveur physique,
    /// 4 serveur de fichiers, 5 NAS.
    #[serde(default)]
    pub source_type: Option<Num>,
    /// Même codage que `source_type` ; sert de repli si celui-ci manque.
    #[serde(default)]
    pub backup_type: Option<Num>,
    /// État de fonctionnement en texte (`backingup`, `waiting`, `unscheduled`…).
    /// Ce sont les valeurs acceptées par le filtre de l'API ; leur présence dans
    /// la réponse n'est pas confirmée par les relevés, d'où l'option.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub device_count: Option<Num>,
    #[serde(default)]
    pub devices: Vec<AbbDevice>,
    /// Prochaine exécution planifiée, en secondes Unix ; 0 ou absent sans planning.
    #[serde(default)]
    pub next_trigger_time: Option<Num>,
    #[serde(default)]
    pub last_result: Option<AbbResult>,
    #[serde(default)]
    pub sched_content: Option<AbbSchedule>,
}

/// Appareil rattaché à une tâche. L'identifiant relie l'appareil à ses exécutions
/// (`SYNO.ActiveBackup.Overview`) ; tout le reste est optionnel pour qu'un champ
/// inattendu ne fasse pas échouer la lecture.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbbDevice {
    #[serde(default)]
    pub device_id: Option<Num>,
    #[serde(default)]
    pub host_name: Option<String>,
}

/// `SYNO.ActiveBackup.Overview&method=list_device_transfer_size` : les exécutions
/// **par appareil** sur une fenêtre de temps — c'est là que vit le rythme d'un
/// portable, pas dans les résultats par tâche.
#[derive(Debug, Default, Deserialize)]
pub struct AbbTransferOverview {
    #[serde(default)]
    pub device_list: Vec<AbbDeviceTransfers>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AbbDeviceTransfers {
    #[serde(default)]
    pub device: AbbDevice,
    #[serde(default)]
    pub transfer_list: Vec<AbbTransfer>,
}

/// Une exécution pour un appareil : mêmes codes de résultat qu'[`AbbResult`],
/// `device_result_id` l'identifie, `result_id` renvoie à l'exécution de la tâche.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbbTransfer {
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub device_result_id: Option<Num>,
    #[serde(default)]
    pub result_id: Option<Num>,
    #[serde(default)]
    pub status: Option<Num>,
    #[serde(default)]
    pub time_start: Option<Num>,
    #[serde(default)]
    pub time_end: Option<Num>,
    #[serde(default)]
    pub transfered_bytes: Option<Num>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbbSchedule {
    /// Sauvegarde continue mise en pause par l'utilisateur.
    #[serde(default)]
    pub is_continuous_paused: Option<Num>,
}

/// Résultat d'une exécution : `last_result` d'une tâche, ou élément de `results`
/// dans `SYNO.ActiveBackup.Log&method=list_result`.
///
/// Les dates sont en secondes Unix — contrairement à Hyper Backup, aucune horloge
/// du NAS n'est nécessaire pour en tirer une ancienneté.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AbbResult {
    /// 2 réussite, 3 réussite partielle, 4 échec, 5 annulation, 6 sans sauvegarde.
    #[serde(default)]
    pub status: Option<Num>,
    /// 1 sauvegarde ; les autres valeurs sont des restaurations, migrations ou
    /// suppressions, dont le résultat ne dit rien de l'état de la sauvegarde.
    #[serde(default)]
    pub job_action: Option<Num>,
    #[serde(default)]
    pub time_start: Option<Num>,
    /// 0 tant que l'exécution est en cours.
    #[serde(default)]
    pub time_end: Option<Num>,
}

/// `SYNO.ActiveBackup.Log&method=list_result` : historique des exécutions.
#[derive(Debug, Default, Deserialize)]
pub struct AbbResultList {
    #[serde(default)]
    pub results: Vec<AbbResult>,
}

/// Nombre tolérant : accepte entier, flottant, booléen ou chaîne numérique.
///
/// Indispensable ici : DSM renvoie les tailles de volume en chaînes (elles
/// dépassent 2⁵³ octets sur un gros NAS) et les températures en entiers, parfois
/// l'inverse selon la version.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub f64);

impl Num {
    /// Valeur brute, ou `default` si le champ était absent.
    pub fn get(value: Option<Num>, default: f64) -> f64 {
        value.map_or(default, |n| n.0)
    }

    /// Vrai si le champ vaut une valeur non nulle — les booléens de DSM sont
    /// tantôt de vrais booléens, tantôt des 0/1.
    pub fn flag(value: Option<Num>) -> bool {
        value.is_some_and(|n| n.0 != 0.0)
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;

        impl Visitor<'_> for NumVisitor {
            type Value = Num;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a numeric string")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Num, E> {
                Ok(Num(if v { 1.0 } else { 0.0 }))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Num, E> {
                v.trim().parse::<f64>().map(Num).map_err(|_| {
                    de::Error::invalid_value(de::Unexpected::Str(v), &"a numeric string")
                })
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Réponse réelle de `SYNO.API.Info`, restreinte aux API qui nous intéressent.
    const API_INFO: &str = r#"{"data":{
      "SYNO.API.Auth":{"path":"entry.cgi","minVersion":1,"maxVersion":7},
      "SYNO.Core.System":{"path":"entry.cgi","minVersion":1,"maxVersion":3},
      "SYNO.Core.System.Utilization":{"path":"entry.cgi","minVersion":1,"maxVersion":1},
      "SYNO.Storage.CGI.Storage":{"path":"entry.cgi","minVersion":1,"maxVersion":1}
    },"success":true}"#;

    fn catalogue(json: &str) -> ApiCatalog {
        serde_json::from_str::<Envelope<ApiCatalog>>(json).unwrap().data.unwrap()
    }

    #[test]
    fn le_catalogue_est_lu_tel_que_le_nas_lannonce() {
        let catalog = catalogue(API_INFO);
        assert!(catalog.contains("SYNO.Core.System"));
        assert!(!catalog.contains("SYNO.Core.System.SystemHealth"));
    }

    #[test]
    fn la_version_souhaitee_est_ramenee_dans_lintervalle_annonce() {
        let catalog = catalogue(API_INFO);

        // La version 6 est recommandée pour l'authentification et disponible ici.
        assert_eq!(catalog.resolve("SYNO.API.Auth", 6).unwrap().version, 6);
        // Une API qui plafonne plus bas doit être appelée à son maximum.
        assert_eq!(catalog.resolve("SYNO.Core.System", 6).unwrap().version, 3);
        // Et jamais en dessous de son minimum.
        assert_eq!(catalog.resolve("SYNO.Core.System", 0).unwrap().version, 1);
    }

    #[test]
    fn un_nas_qui_plafonne_lauthentification_en_version_3_reste_utilisable() {
        // Cas d'un DSM 6 : la version 6 de SYNO.API.Auth n'existe pas encore.
        let catalog = catalogue(
            r#"{"data":{"SYNO.API.Auth":{"path":"auth.cgi","minVersion":1,"maxVersion":3}},
                "success":true}"#,
        );
        let endpoint = catalog.resolve("SYNO.API.Auth", 6).unwrap();
        assert_eq!(endpoint.version, 3);
        assert_eq!(endpoint.path, "auth.cgi");
    }

    #[test]
    fn une_api_absente_du_catalogue_ne_se_resout_pas() {
        assert!(catalogue(API_INFO).resolve("SYNO.Backup.Task", 1).is_none());
    }

    #[test]
    fn un_intervalle_de_versions_incoherent_retombe_sur_la_borne_basse() {
        let catalog = catalogue(
            r#"{"data":{"X":{"path":"entry.cgi","minVersion":4,"maxVersion":1}},"success":true}"#,
        );
        assert_eq!(catalog.resolve("X", 2).unwrap().version, 4);
    }

    #[test]
    fn un_chemin_dangereux_annonce_par_le_nas_est_refuse() {
        // Le chemin vient du réseau : il ne doit pas pouvoir détourner une requête
        // authentifiée vers un autre hôte ni remonter hors de /webapi.
        for chemin in [
            "../../etc/passwd",
            "/absolu.cgi",
            "http://ailleurs.example/collecte",
            "entry.cgi?api=autre",
            "",
            "   ",
        ] {
            let json = format!(
                r#"{{"data":{{"X":{{"path":"{chemin}","minVersion":1,"maxVersion":1}}}},"success":true}}"#
            );
            assert!(
                catalogue(&json).resolve("X", 1).is_none(),
                "« {chemin} » aurait dû être refusé"
            );
        }
    }

    #[test]
    fn un_chemin_de_paquet_reste_accepte() {
        let catalog = catalogue(
            r#"{"data":{"X":{"path":"VideoStation/info.cgi","minVersion":1,"maxVersion":1}},
                "success":true}"#,
        );
        assert_eq!(catalog.resolve("X", 1).unwrap().path, "VideoStation/info.cgi");
    }

    #[test]
    fn une_enveloppe_en_succes_na_pas_de_code_derreur() {
        let envelope: Envelope<serde_json::Value> =
            serde_json::from_str(r#"{"data":{},"success":true}"#).unwrap();
        assert_eq!(envelope.error_code(), None);
    }

    #[test]
    fn une_enveloppe_en_echec_expose_son_code() {
        let envelope: Envelope<serde_json::Value> =
            serde_json::from_str(r#"{"error":{"code":403},"success":false}"#).unwrap();
        assert_eq!(envelope.error_code(), Some(403));
    }

    #[test]
    fn un_echec_sans_objet_derreur_reste_un_echec() {
        let envelope: Envelope<serde_json::Value> =
            serde_json::from_str(r#"{"success":false}"#).unwrap();
        assert_eq!(envelope.error_code(), Some(100));
    }

    #[test]
    fn les_nombres_arrivent_en_chaines_comme_en_entiers() {
        #[derive(Deserialize)]
        struct Taille {
            total: Num,
            temp: Num,
            actif: Num,
        }
        let taille: Taille =
            serde_json::from_str(r#"{"total":"5343156371456","temp":38,"actif":true}"#).unwrap();
        assert_eq!(taille.total.0, 5_343_156_371_456.0);
        assert_eq!(taille.temp.0, 38.0);
        assert_eq!(taille.actif.0, 1.0);
    }

    #[test]
    fn le_debug_de_la_reponse_de_connexion_ne_laisse_rien_fuir() {
        let data: LoginData =
            serde_json::from_str(r#"{"sid":"SECRET-SID","synotoken":"SECRET-JETON"}"#).unwrap();
        let rendu = format!("{data:?}");
        assert!(!rendu.contains("SECRET-SID"), "{rendu}");
        assert!(!rendu.contains("SECRET-JETON"), "{rendu}");
    }
}
