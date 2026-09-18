//! Moteur de profils de collecte.
//!
//! Un profil décrit, en YAML, les OID à lire sur une famille d'équipements et la
//! façon de les étiqueter. Ce fichier définit le format — contrat avec l'interface
//! web — ainsi que la sélection automatique du profil le plus spécifique.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use tracing::error;

use super::oid::ObjectId;
use super::pattern::Pattern;
use super::value::SnmpValue;

/// Nombre maximal de lignes conservées par métrique tabulaire, faute de réglage
/// explicite dans le profil. Un châssis de 48 ports en produit une cinquantaine ;
/// au-delà de 512, c'est un équipement pathologique ou une MIB mal choisie.
pub const DEFAULT_MAX_ROWS: usize = 512;

/// Nature de la métrique, telle qu'écrite dans le YAML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKindSpec {
    Counter,
    Gauge,
}

impl From<MetricKindSpec> for dumbmonit_proto::MetricKind {
    fn from(kind: MetricKindSpec) -> Self {
        match kind {
            MetricKindSpec::Counter => Self::Counter,
            MetricKindSpec::Gauge => Self::Gauge,
        }
    }
}

/// Critères de reconnaissance d'un équipement.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    /// Préfixes de `sysObjectID`, ou `"*"` pour le repli universel.
    #[serde(default)]
    pub sysobjectid: Vec<String>,
    /// Motifs appliqués à `sysDescr`. Si la liste est non vide, au moins un motif
    /// doit correspondre pour que le profil soit retenu.
    #[serde(default)]
    pub sysdescr: Vec<Pattern>,
    /// Départage deux profils de spécificité d'OID identique. Sert essentiellement
    /// à ordonner les profils de repli entre eux.
    #[serde(default)]
    pub priority: i32,
}

/// Une condition portant sur la valeur d'une colonne, à l'index de la ligne examinée.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidCondition {
    pub oid: ObjectId,
    /// Valeurs acceptées. Les entiers sont comparés numériquement, les chaînes au
    /// rendu texte de la valeur — ce qui permet de viser un `hrStorageType`, dont la
    /// valeur est elle-même un OID.
    pub values: Vec<ScalarMatch>,
}

impl OidCondition {
    fn is_satisfied_by(&self, value: &SnmpValue) -> bool {
        self.values.iter().any(|expected| expected.matches(value))
    }
}

/// Une valeur attendue, écrite en YAML soit comme un entier, soit comme une chaîne.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ScalarMatch {
    Number(i64),
    Text(String),
}

impl ScalarMatch {
    fn matches(&self, value: &SnmpValue) -> bool {
        match self {
            Self::Number(expected) => value.as_f64().is_some_and(|actual| {
                // Comparaison exacte après conversion : les codes d'énumération SNMP
                // sont de petits entiers, jamais affectés par l'arrondi.
                actual == *expected as f64
            }),
            Self::Text(expected) => value.as_label().is_some_and(|actual| actual == *expected),
        }
    }
}

/// Conjonction de conditions : la ligne n'est écartée que si toutes se vérifient.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionGroup {
    pub conditions: Vec<OidCondition>,
}

/// Filtre de cardinalité, appliqué ligne par ligne aux métriques tabulaires.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    /// Borne dure du nombre de lignes conservées pour une métrique.
    #[serde(default)]
    pub max_rows: Option<usize>,
    /// Écarte la ligne si l'une de ses étiquettes correspond à l'un des motifs.
    #[serde(default)]
    pub drop_when_label_matches: BTreeMap<String, Vec<Pattern>>,
    /// Écarte la ligne si l'une de ces conditions se vérifie.
    #[serde(default)]
    pub drop_when_oid_equals: Vec<OidCondition>,
    /// Écarte la ligne si toutes les conditions d'un même groupe se vérifient.
    /// Sert aux critères composés — « jamais montée » = état bas *et* jamais changé.
    #[serde(default)]
    pub drop_when_all: Vec<ConditionGroup>,
    /// Ne conserve la ligne que si toutes ces conditions se vérifient.
    #[serde(default)]
    pub keep_only_when_oid_equals: Vec<OidCondition>,
}

impl Filters {
    /// Tous les OID que le filtre a besoin de connaître, à parcourir une seule fois.
    pub fn required_oids(&self) -> Vec<&ObjectId> {
        let mut oids: Vec<&ObjectId> = Vec::new();
        for condition in &self.drop_when_oid_equals {
            oids.push(&condition.oid);
        }
        for condition in &self.keep_only_when_oid_equals {
            oids.push(&condition.oid);
        }
        for group in &self.drop_when_all {
            for condition in &group.conditions {
                oids.push(&condition.oid);
            }
        }
        oids
    }

    /// Décide du sort d'une ligne de table.
    ///
    /// `column` donne la valeur d'une colonne à l'index de la ligne ; `labels` donne
    /// les étiquettes déjà résolues. Une colonne absente ne peut satisfaire aucune
    /// condition : on ne rejette jamais une ligne sur une information manquante,
    /// sauf liste blanche, où l'absence de preuve vaut absence.
    pub fn keeps<'a>(
        &self,
        labels: &BTreeMap<String, String>,
        column: impl Fn(&ObjectId) -> Option<&'a SnmpValue>,
    ) -> bool {
        for (label, patterns) in &self.drop_when_label_matches {
            if let Some(actual) = labels.get(label)
                && patterns.iter().any(|pattern| pattern.is_match(actual))
            {
                return false;
            }
        }

        for condition in &self.drop_when_oid_equals {
            if let Some(value) = column(&condition.oid)
                && condition.is_satisfied_by(value)
            {
                return false;
            }
        }

        for group in &self.drop_when_all {
            if !group.conditions.is_empty()
                && group.conditions.iter().all(|condition| {
                    column(&condition.oid).is_some_and(|value| condition.is_satisfied_by(value))
                })
            {
                return false;
            }
        }

        for condition in &self.keep_only_when_oid_equals {
            if !column(&condition.oid).is_some_and(|value| condition.is_satisfied_by(value)) {
                return false;
            }
        }

        true
    }
}

/// Une métrique à collecter.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    /// Nom de la série, en `snake_case` et sans préfixe.
    pub name: String,
    pub oid: ObjectId,
    pub kind: MetricKindSpec,
    /// `true` : parcours du sous-arbre (table). `false` : lecture d'une seule instance.
    #[serde(default)]
    pub walk: bool,
    /// Étiquettes indexées : nom d'étiquette vers OID de la colonne qui la porte.
    #[serde(default)]
    pub labels: BTreeMap<String, ObjectId>,
    /// Étiquettes constantes, utiles pour distinguer deux métriques issues de la
    /// même table mais filtrées différemment.
    #[serde(default)]
    pub static_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub scale: Option<f64>,
    /// Multiplie la valeur par celle d'une autre colonne au même index.
    ///
    /// Indispensable à HOST-RESOURCES, où une taille de volume ne s'obtient qu'en
    /// multipliant un nombre de blocs par la taille d'un bloc.
    #[serde(default)]
    pub multiply_by: Option<ObjectId>,
    /// Filtre propre à cette métrique ; à défaut, celui du profil s'applique.
    #[serde(default)]
    pub filters: Option<Filters>,
}

/// Un profil de collecte, tel qu'écrit dans `profiles/`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(rename = "match", default)]
    pub match_spec: MatchSpec,
    /// Profils dont les métriques sont ajoutées à celles-ci, résolus récursivement.
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub filters: Option<Filters>,
}

impl Profile {
    pub fn parse(source: &str) -> Result<Self, String> {
        let profile: Profile =
            serde_yaml_ng::from_str(source).map_err(|error| error.to_string())?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("field \"id\" is required".to_string());
        }
        for prefix in &self.match_spec.sysobjectid {
            if prefix != "*" && prefix.parse::<ObjectId>().is_err() {
                return Err(format!(
                    "profile \"{}\": \"{prefix}\" is neither an OID nor \"*\"",
                    self.id
                ));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for metric in &self.metrics {
            if !seen.insert(&metric.name) {
                return Err(format!(
                    "profile \"{}\": metric \"{}\" is declared twice",
                    self.id, metric.name
                ));
            }
            // Un OID d'instance se termine par « .0 » ; un OID de colonne, jamais.
            // Confondre les deux donne une métrique silencieusement vide, faute la
            // plus fréquente à l'écriture d'un profil.
            let is_instance = metric.oid.arcs().last() == Some(&0);
            if metric.walk == is_instance {
                return Err(format!(
                    "profile \"{}\", metric \"{}\": \"walk: {}\" is incompatible with \
                     OID \"{}\"",
                    self.id, metric.name, metric.walk, metric.oid
                ));
            }
        }
        Ok(())
    }

    /// Spécificité de la correspondance avec un équipement, `None` si le profil ne
    /// correspond pas.
    ///
    /// Le score est le nombre d'arcs du plus long préfixe de `sysObjectID` reconnu ;
    /// `"*"` vaut zéro. Un profil qui exige un `sysDescr` et l'obtient gagne un point,
    /// ce qui le place devant un profil de même spécificité d'OID mais moins exigeant.
    pub fn specificity(
        &self,
        sysobjectid: Option<&ObjectId>,
        sysdescr: Option<&str>,
    ) -> Option<i32> {
        let mut requires_descr = false;
        if !self.match_spec.sysdescr.is_empty() {
            requires_descr = true;
            let text = sysdescr?;
            if !self.match_spec.sysdescr.iter().any(|pattern| pattern.is_match(text)) {
                return None;
            }
        }

        let mut best: Option<i32> = None;
        for prefix in &self.match_spec.sysobjectid {
            let score = if prefix == "*" {
                Some(0)
            } else {
                // `validate` a déjà rejeté les OID illisibles ; ici, on se contente
                // d'ignorer le critère plutôt que d'écarter tout le profil.
                match (prefix.parse::<ObjectId>(), sysobjectid) {
                    // Un préfixe plus long l'emporte : 1.3.6.1.4.1.9.1.516 (un modèle
                    // précis) doit passer devant 1.3.6.1.4.1.9 (tout le catalogue).
                    (Ok(prefix), Some(actual)) if actual.starts_with(&prefix) => {
                        Some(i32::try_from(prefix.arcs().len()).unwrap_or(i32::MAX) * 10)
                    }
                    _ => None,
                }
            };
            if let Some(score) = score {
                best = Some(best.map_or(score, |current: i32| current.max(score)));
            }
        }

        // Un profil sans critère d'OID mais dont le sysDescr correspond reste éligible.
        if best.is_none() && self.match_spec.sysobjectid.is_empty() && requires_descr {
            best = Some(0);
        }

        best.map(|score| score + i32::from(requires_descr) + self.match_spec.priority)
    }
}

/// L'ensemble des profils connus du serveur.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    profiles: Vec<Profile>,
}

impl Catalog {
    pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut profiles = Vec::new();
        for (origin, source) in sources {
            match Profile::parse(source) {
                Ok(profile) => profiles.push(profile),
                // Un profil illisible ne doit pas empêcher le serveur de démarrer :
                // les autres équipements restent surveillés.
                Err(error) => error!(profile = origin, %error, "profil ignoré"),
            }
        }
        Self { profiles }
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.profiles.iter().map(|profile| profile.id.as_str()).collect()
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Choisit le profil le plus spécifique pour un équipement identifié.
    ///
    /// À score égal, l'identifiant départage : la sélection reste ainsi reproductible
    /// d'un démarrage à l'autre, ce qui évite qu'une cible change de profil toute seule.
    pub fn select(
        &self,
        sysobjectid: Option<&ObjectId>,
        sysdescr: Option<&str>,
    ) -> Option<&Profile> {
        self.profiles
            .iter()
            .filter_map(|profile| {
                profile.specificity(sysobjectid, sysdescr).map(|score| (score, profile))
            })
            .max_by(|(left_score, left), (right_score, right)| {
                left_score.cmp(right_score).then_with(|| right.id.cmp(&left.id))
            })
            .map(|(_, profile)| profile)
    }

    /// Développe un profil et ses inclusions en une liste de métriques à collecter.
    ///
    /// Chaque métrique est rendue avec le filtre qui la concerne — le sien s'il existe,
    /// sinon celui du profil qui la déclare — pour que l'inclusion n'aille pas
    /// appliquer les filtres d'IF-MIB aux volumes de HOST-RESOURCES.
    pub fn resolve(&self, id: &str) -> Result<Vec<ResolvedMetric>, String> {
        let mut resolved = Vec::new();
        let mut visited = Vec::new();
        self.resolve_into(id, &mut resolved, &mut visited)?;
        Ok(resolved)
    }

    fn resolve_into(
        &self,
        id: &str,
        output: &mut Vec<ResolvedMetric>,
        visited: &mut Vec<String>,
    ) -> Result<(), String> {
        // Deux profils qui s'incluent mutuellement boucleraient sans cette garde.
        if visited.iter().any(|seen| seen == id) {
            return Ok(());
        }
        visited.push(id.to_string());

        let profile = self.get(id).ok_or_else(|| {
            format!("unknown profile \"{id}\" (known: {})", self.ids().join(", "))
        })?;

        for included in &profile.include {
            self.resolve_into(included, output, visited)?;
        }

        for metric in &profile.metrics {
            // Une métrique déjà apportée par une inclusion n'est pas redéfinie : le
            // profil le plus profond gagne, ce qui rend l'inclusion prévisible.
            if output.iter().any(|existing| existing.metric.name == metric.name) {
                continue;
            }
            let filters = metric.filters.clone().or_else(|| profile.filters.clone());
            output.push(ResolvedMetric { metric: metric.clone(), filters });
        }
        Ok(())
    }
}

/// Une métrique accompagnée du filtre effectif à lui appliquer.
#[derive(Debug, Clone)]
pub struct ResolvedMetric {
    pub metric: Metric,
    pub filters: Option<Filters>,
}

impl ResolvedMetric {
    pub fn max_rows(&self) -> usize {
        self.filters.as_ref().and_then(|filters| filters.max_rows).unwrap_or(DEFAULT_MAX_ROWS)
    }
}

// ---------------------------------------------------------------------------
// Catalogue embarqué
// ---------------------------------------------------------------------------

/// Les profils livrés avec le produit, incorporés au binaire à la compilation.
///
/// L'image finale est une `scratch` sans système de fichiers : rien ne peut être lu
/// depuis `profiles/` à l'exécution. `include_str!` garantit en outre qu'un fichier
/// renommé ou supprimé casse la compilation plutôt que de vider silencieusement le
/// catalogue en production.
const EMBEDDED: &[(&str, &str)] = &[
    ("system.yaml", include_str!("../../../../profiles/system.yaml")),
    ("if-mib.yaml", include_str!("../../../../profiles/if-mib.yaml")),
    ("host-resources.yaml", include_str!("../../../../profiles/host-resources.yaml")),
    ("ups.yaml", include_str!("../../../../profiles/ups.yaml")),
    ("printer.yaml", include_str!("../../../../profiles/printer.yaml")),
];

/// Catalogue partagé, construit une seule fois.
pub fn embedded() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog::from_sources(EMBEDDED.iter().copied()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IF_MIB: &str = r#"
id: if-mib
name: Interfaces réseau
match:
  sysobjectid: ["*"]
  priority: 10
include: [system]
metrics:
  - name: if_octets_in
    oid: 1.3.6.1.2.1.31.1.1.1.6
    kind: counter
    walk: true
    labels:
      ifname: 1.3.6.1.2.1.31.1.1.1.1
    scale: 1.0
filters:
  max_rows: 64
  drop_when_label_matches:
    ifname: ["^lo$", "^veth"]
"#;

    const SYSTEM: &str = r#"
id: system
name: Système
match:
  sysobjectid: ["*"]
metrics:
  - name: system_uptime_seconds
    oid: 1.3.6.1.2.1.1.3.0
    kind: gauge
    scale: 0.01
"#;

    const CISCO: &str = r#"
id: cisco
name: Cisco
match:
  sysobjectid: ["1.3.6.1.4.1.9"]
metrics: []
"#;

    const CISCO_CATALYST: &str = r#"
id: cisco-catalyst
name: Catalyst
match:
  sysobjectid: ["1.3.6.1.4.1.9.1.516"]
metrics: []
"#;

    const LINUX: &str = r#"
id: linux
name: Linux
match:
  sysobjectid: ["1.3.6.1.4.1.8072"]
  sysdescr: ["^Linux"]
metrics: []
"#;

    fn catalog() -> Catalog {
        Catalog::from_sources([
            ("system", SYSTEM),
            ("if-mib", IF_MIB),
            ("cisco", CISCO),
            ("cisco-catalyst", CISCO_CATALYST),
            ("linux", LINUX),
        ])
    }

    fn oid(raw: &str) -> ObjectId {
        raw.parse().unwrap()
    }

    #[test]
    fn analyse_du_format_documente() {
        let profile = Profile::parse(IF_MIB).unwrap();
        assert_eq!(profile.id, "if-mib");
        assert_eq!(profile.include, vec!["system"]);
        let metric = &profile.metrics[0];
        assert_eq!(metric.name, "if_octets_in");
        assert_eq!(metric.oid.to_string(), "1.3.6.1.2.1.31.1.1.1.6");
        assert_eq!(metric.kind, MetricKindSpec::Counter);
        assert!(metric.walk);
        assert_eq!(metric.scale, Some(1.0));
        assert_eq!(
            metric.labels.get("ifname").map(ToString::to_string).as_deref(),
            Some("1.3.6.1.2.1.31.1.1.1.1")
        );
        assert_eq!(profile.filters.unwrap().max_rows, Some(64));
    }

    #[test]
    fn un_champ_inconnu_est_refuse() {
        let source = "id: x\nname: X\nmetrics: []\nmetriques: []\n";
        assert!(Profile::parse(source).is_err());
    }

    #[test]
    fn un_oid_invalide_est_refuse_a_l_analyse() {
        let source = "id: x\nname: X\nmetrics:\n  - name: m\n    oid: 1.3.six.1\n    kind: gauge\n";
        assert!(Profile::parse(source).is_err());
    }

    #[test]
    fn un_motif_de_correspondance_invalide_est_refuse() {
        let source = "id: x\nname: X\nmatch:\n  sysobjectid: [\"pas-un-oid\"]\nmetrics: []\n";
        assert!(Profile::parse(source).is_err());
    }

    #[test]
    fn walk_et_oid_doivent_etre_coherents() {
        // Un OID d'instance parcouru comme une table, et l'inverse.
        let instance_en_walk = "id: x\nname: X\nmetrics:\n  - name: m\n    oid: 1.3.6.1.2.1.1.3.0\n    kind: gauge\n    walk: true\n";
        assert!(Profile::parse(instance_en_walk).is_err());
        let colonne_en_get = "id: x\nname: X\nmetrics:\n  - name: m\n    oid: 1.3.6.1.2.1.2.2.1.10\n    kind: counter\n";
        assert!(Profile::parse(colonne_en_get).is_err());

        // Une étiquette portée par une instance unique reste légitime : c'est ainsi
        // que le profil « system » expose sysName et sysLocation.
        let scalaire_etiquete = "id: x\nname: X\nmetrics:\n  - name: m\n    oid: 1.3.6.1.2.1.1.7.0\n    kind: gauge\n    labels:\n      sysname: 1.3.6.1.2.1.1.5.0\n";
        assert!(Profile::parse(scalaire_etiquete).is_ok());
    }

    #[test]
    fn le_prefixe_le_plus_long_l_emporte() {
        let catalog = catalog();
        let chosen = catalog.select(Some(&oid("1.3.6.1.4.1.9.1.516")), None).unwrap();
        assert_eq!(chosen.id, "cisco-catalyst");

        let chosen = catalog.select(Some(&oid("1.3.6.1.4.1.9.1.999")), None).unwrap();
        assert_eq!(chosen.id, "cisco");
    }

    #[test]
    fn le_joker_est_le_dernier_recours() {
        let catalog = catalog();
        let chosen = catalog.select(Some(&oid("1.3.6.1.4.1.42424.1")), None).unwrap();
        // system et if-mib correspondent tous deux via « * » ; la priorité tranche.
        assert_eq!(chosen.id, "if-mib");
    }

    #[test]
    fn le_sysdescr_est_une_condition_necessaire() {
        let catalog = catalog();
        let net_snmp = oid("1.3.6.1.4.1.8072.3.2.10");
        assert_eq!(catalog.select(Some(&net_snmp), Some("Linux nas 6.1.0")).unwrap().id, "linux");
        // Même sysObjectID, mais un sysDescr qui ne correspond pas : le profil Linux
        // est écarté et l'on retombe sur le repli.
        assert_eq!(catalog.select(Some(&net_snmp), Some("Windows")).unwrap().id, "if-mib");
        assert_eq!(catalog.select(Some(&net_snmp), None).unwrap().id, "if-mib");
    }

    #[test]
    fn selection_sans_identification_du_tout() {
        let catalog = catalog();
        assert_eq!(catalog.select(None, None).unwrap().id, "if-mib");
    }

    #[test]
    fn la_selection_est_reproductible() {
        let catalog = catalog();
        let first = catalog
            .select(Some(&oid("1.3.6.1.4.1.9.1.516")), Some("Cisco IOS"))
            .unwrap()
            .id
            .clone();
        for _ in 0..10 {
            let again =
                catalog.select(Some(&oid("1.3.6.1.4.1.9.1.516")), Some("Cisco IOS")).unwrap();
            assert_eq!(again.id, first);
        }
    }

    #[test]
    fn les_inclusions_sont_developpees() {
        let metrics = catalog().resolve("if-mib").unwrap();
        let names: Vec<&str> = metrics.iter().map(|m| m.metric.name.as_str()).collect();
        assert_eq!(names, vec!["system_uptime_seconds", "if_octets_in"]);
        // La métrique héritée conserve le filtre de son profil d'origine (aucun),
        // celle d'IF-MIB reçoit celui d'IF-MIB.
        assert!(metrics[0].filters.is_none());
        assert_eq!(metrics[1].max_rows(), 64);
        assert_eq!(metrics[0].max_rows(), DEFAULT_MAX_ROWS);
    }

    #[test]
    fn une_inclusion_circulaire_ne_boucle_pas() {
        let a = "id: a\nname: A\ninclude: [b]\nmetrics: []\n";
        let b = "id: b\nname: B\ninclude: [a]\nmetrics: []\n";
        let catalog = Catalog::from_sources([("a", a), ("b", b)]);
        assert!(catalog.resolve("a").is_ok());
    }

    #[test]
    fn une_inclusion_inconnue_est_signalee() {
        let a = "id: a\nname: A\ninclude: [absent]\nmetrics: []\n";
        let catalog = Catalog::from_sources([("a", a)]);
        assert!(catalog.resolve("a").is_err());
        assert!(catalog.resolve("inexistant").is_err());
    }

    #[test]
    fn filtre_par_motif_d_etiquette() {
        let filters = Profile::parse(IF_MIB).unwrap().filters.unwrap();
        let mut labels = BTreeMap::new();
        labels.insert("ifname".to_string(), "lo".to_string());
        assert!(!filters.keeps(&labels, |_| None));

        labels.insert("ifname".to_string(), "veth1234".to_string());
        assert!(!filters.keeps(&labels, |_| None));

        labels.insert("ifname".to_string(), "eth0".to_string());
        assert!(filters.keeps(&labels, |_| None));
    }

    #[test]
    fn filtre_par_valeur_de_colonne() {
        let source = r#"
id: x
name: X
metrics: []
filters:
  drop_when_oid_equals:
    - oid: 1.3.6.1.2.1.2.2.1.7
      values: [2, 3]
"#;
        let filters = Profile::parse(source).unwrap().filters.unwrap();
        let admin = oid("1.3.6.1.2.1.2.2.1.7");
        let labels = BTreeMap::new();

        let down = SnmpValue::Integer(2);
        assert!(!filters.keeps(&labels, |asked| (*asked == admin).then_some(&down)));

        let up = SnmpValue::Integer(1);
        assert!(filters.keeps(&labels, |asked| (*asked == admin).then_some(&up)));

        // Colonne absente : on ne rejette pas sur une information manquante.
        assert!(filters.keeps(&labels, |_| None));
        assert_eq!(filters.required_oids(), vec![&admin]);
    }

    #[test]
    fn filtre_compose_exige_toutes_les_conditions() {
        let source = r#"
id: x
name: X
metrics: []
filters:
  drop_when_all:
    - conditions:
        - oid: 1.3.6.1.2.1.2.2.1.8
          values: [2]
        - oid: 1.3.6.1.2.1.2.2.1.9
          values: [0]
"#;
        let filters = Profile::parse(source).unwrap().filters.unwrap();
        let oper = oid("1.3.6.1.2.1.2.2.1.8");
        let last_change = oid("1.3.6.1.2.1.2.2.1.9");
        let labels = BTreeMap::new();

        let down = SnmpValue::Integer(2);
        let never = SnmpValue::Timeticks(0);
        let recent = SnmpValue::Timeticks(4200);

        // Basse et jamais montée : écartée.
        assert!(!filters.keeps(&labels, |asked| {
            if *asked == oper {
                Some(&down)
            } else if *asked == last_change {
                Some(&never)
            } else {
                None
            }
        }));

        // Basse mais déjà montée un jour : conservée, la panne est intéressante.
        assert!(filters.keeps(&labels, |asked| {
            if *asked == oper {
                Some(&down)
            } else if *asked == last_change {
                Some(&recent)
            } else {
                None
            }
        }));
    }

    #[test]
    fn liste_blanche_sur_un_oid_en_valeur() {
        let source = r#"
id: x
name: X
metrics: []
filters:
  keep_only_when_oid_equals:
    - oid: 1.3.6.1.2.1.25.2.3.1.2
      values: ["1.3.6.1.2.1.25.2.1.4"]
"#;
        let filters = Profile::parse(source).unwrap().filters.unwrap();
        let type_oid = oid("1.3.6.1.2.1.25.2.3.1.2");
        let labels = BTreeMap::new();

        let fixed_disk = SnmpValue::Oid(oid("1.3.6.1.2.1.25.2.1.4"));
        assert!(filters.keeps(&labels, |asked| (*asked == type_oid).then_some(&fixed_disk)));

        let ram = SnmpValue::Oid(oid("1.3.6.1.2.1.25.2.1.2"));
        assert!(!filters.keeps(&labels, |asked| (*asked == type_oid).then_some(&ram)));

        // Sans la colonne, la liste blanche ne peut pas être satisfaite.
        assert!(!filters.keeps(&labels, |_| None));
    }
}
