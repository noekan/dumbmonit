//! Routage par étiquettes : ce qu'un canal de notification accepte de recevoir.
//!
//! Un homelab a des lieux et des rôles (`site=cellar`, `role=storage`). Sans
//! filtre, chaque canal reçoit tout ce que la politique laisse passer, si bien
//! que le téléphone de la nuit sonne pour un conteneur de test. Le filtre décrit
//! ici répond à une seule question — « ce canal veut-il de cette alerte ? » — et
//! se veut lisible à voix haute : « les avis et au-dessus, venant des
//! équipements étiquetés site=cellar, sauf ceux étiquetés role=lab ».
//!
//! Volontairement pauvre : égalité exacte, pas d'expression régulière, pas de
//! parenthèses. Un filtre que l'on ne sait pas relire fait taire une alerte sans
//! que personne ne s'en aperçoive, ce qui est exactement ce qu'un outil de
//! supervision ne doit jamais faire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Ce qu'une alerte donne à voir au filtre.
///
/// Les étiquettes sont celles de l'équipement (`site`, `role`…), pas les
/// `tag_*` de la série : c'est ce que l'utilisateur a saisi dans la fiche de
/// l'équipement, et c'est donc ce qu'il tape dans le filtre.
#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    /// `uid` de la règle qui a déclenché.
    pub rule_uid: &'a str,
    /// Collecteur de l'équipement (`snmp`, `proxmox`, `agent`…).
    pub kind: &'a str,
    /// Étiquettes de l'équipement.
    pub tags: &'a BTreeMap<String, String>,
}

/// Une condition élémentaire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "lowercase")]
pub enum Condition {
    /// L'équipement porte cette étiquette, avec cette valeur exacte.
    Tag { key: String, value: String },
    /// L'équipement est interrogé par ce collecteur.
    Kind { value: String },
    /// L'alerte vient de cette règle (`uid`).
    Rule { value: String },
}

impl Condition {
    fn holds(&self, ctx: &MatchContext<'_>) -> bool {
        match self {
            Self::Tag { key, value } => {
                ctx.tags.get(key.as_str()).map(String::as_str) == Some(value)
            }
            Self::Kind { value } => ctx.kind == value,
            Self::Rule { value } => ctx.rule_uid == value,
        }
    }

    /// Famille de la condition, pour le regroupement : deux conditions de la
    /// même famille se lisent en OU, deux familles différentes en ET.
    fn family(&self) -> (&'static str, &str) {
        match self {
            Self::Tag { key, .. } => ("tag", key.as_str()),
            Self::Kind { .. } => ("kind", ""),
            Self::Rule { .. } => ("rule", ""),
        }
    }
}

/// Filtre d'un canal. Vide : le canal reçoit tout, comme avant.
///
/// `include` se lit « OU à l'intérieur d'une famille, ET entre familles » :
/// `site=cellar`, `site=attic`, `role=storage` signifie « (cave OU grenier) ET
/// stockage ». C'est la seule forme dont un homelab a besoin, et la seule qui
/// tienne dans une phrase.
///
/// `exclude` prime toujours : ce qui y correspond est écarté, même si
/// `include` l'acceptait.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelMatcher {
    pub include: Vec<Condition>,
    pub exclude: Vec<Condition>,
}

impl ChannelMatcher {
    /// Vrai quand le filtre ne dit rien : le canal reçoit tout.
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Nombre total de conditions, pour borner ce que l'API accepte.
    pub fn len(&self) -> usize {
        self.include.len() + self.exclude.len()
    }

    /// Le filtre réduit à ce qui ne dépend que de l'équipement.
    ///
    /// C'est exactement ce qu'un aperçu peut évaluer : une liste d'équipements
    /// ne porte aucune alerte, donc aucune règle. Les conditions de règle sont
    /// dites à part dans l'interface (« …et seulement la règle disk_full »),
    /// jamais silencieusement ignorées.
    pub fn device_part(&self) -> Self {
        let keep = |conditions: &[Condition]| -> Vec<Condition> {
            conditions
                .iter()
                .filter(|condition| !matches!(condition, Condition::Rule { .. }))
                .cloned()
                .collect()
        };
        Self { include: keep(&self.include), exclude: keep(&self.exclude) }
    }

    /// Règles citées par le filtre : celles exigées, puis celles écartées.
    pub fn rule_values(&self) -> (Vec<&str>, Vec<&str>) {
        fn pick(conditions: &[Condition]) -> Vec<&str> {
            conditions
                .iter()
                .filter_map(|condition| match condition {
                    Condition::Rule { value } => Some(value.as_str()),
                    _ => None,
                })
                .collect()
        }
        (pick(&self.include), pick(&self.exclude))
    }

    /// Vrai si le canal veut de cette alerte.
    pub fn accepts(&self, ctx: &MatchContext<'_>) -> bool {
        if self.exclude.iter().any(|condition| condition.holds(ctx)) {
            return false;
        }
        if self.include.is_empty() {
            return true;
        }
        // Chaque famille présente doit être satisfaite par au moins une de ses
        // conditions.
        let mut families: Vec<(&'static str, &str)> = Vec::new();
        for condition in &self.include {
            let family = condition.family();
            if !families.contains(&family) {
                families.push(family);
            }
        }
        families.into_iter().all(|family| {
            self.include
                .iter()
                .filter(|condition| condition.family() == family)
                .any(|condition| condition.holds(ctx))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    fn tag(key: &str, value: &str) -> Condition {
        Condition::Tag { key: key.to_string(), value: value.to_string() }
    }

    #[test]
    fn un_filtre_vide_laisse_tout_passer() {
        let tags = tags(&[("site", "cellar")]);
        let ctx = MatchContext { rule_uid: "cpu_high", kind: "snmp", tags: &tags };
        assert!(ChannelMatcher::default().accepts(&ctx), "nothing changes for existing channels");
        assert!(ChannelMatcher::default().is_empty());
    }

    #[test]
    fn une_etiquette_exigee_selectionne_et_ecarte() {
        let matcher = ChannelMatcher { include: vec![tag("site", "cellar")], exclude: Vec::new() };

        let cave = tags(&[("site", "cellar"), ("role", "storage")]);
        assert!(matcher.accepts(&MatchContext { rule_uid: "cpu_high", kind: "snmp", tags: &cave }));

        let grenier = tags(&[("site", "attic")]);
        assert!(!matcher.accepts(&MatchContext {
            rule_uid: "cpu_high",
            kind: "snmp",
            tags: &grenier
        }));

        // Un équipement sans étiquette du tout n'est pas « dans la cave ».
        let nues = tags(&[]);
        assert!(!matcher.accepts(&MatchContext {
            rule_uid: "cpu_high",
            kind: "snmp",
            tags: &nues
        }));
    }

    #[test]
    fn deux_valeurs_de_la_meme_etiquette_se_lisent_en_ou() {
        let matcher = ChannelMatcher {
            include: vec![tag("site", "cellar"), tag("site", "attic")],
            exclude: Vec::new(),
        };
        for site in ["cellar", "attic"] {
            let t = tags(&[("site", site)]);
            assert!(
                matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &t }),
                "{site} should match"
            );
        }
        let ailleurs = tags(&[("site", "garage")]);
        assert!(!matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &ailleurs }));
    }

    #[test]
    fn deux_etiquettes_differentes_se_lisent_en_et() {
        let matcher = ChannelMatcher {
            include: vec![tag("site", "cellar"), tag("role", "storage")],
            exclude: Vec::new(),
        };
        let nas = tags(&[("site", "cellar"), ("role", "storage")]);
        assert!(matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &nas }));
        // Le bon lieu, le mauvais rôle : écarté.
        let switch = tags(&[("site", "cellar"), ("role", "network")]);
        assert!(!matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &switch }));
    }

    #[test]
    fn une_exclusion_prime_sur_une_inclusion() {
        let matcher = ChannelMatcher {
            include: vec![tag("site", "cellar")],
            exclude: vec![tag("role", "lab")],
        };
        let labo = tags(&[("site", "cellar"), ("role", "lab")]);
        assert!(!matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &labo }));
        let nas = tags(&[("site", "cellar"), ("role", "storage")]);
        assert!(matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &nas }));
    }

    #[test]
    fn une_exclusion_seule_laisse_passer_le_reste() {
        let matcher = ChannelMatcher {
            include: Vec::new(),
            exclude: vec![Condition::Kind { value: "proxmox".to_string() }],
        };
        let t = tags(&[]);
        assert!(!matcher.accepts(&MatchContext { rule_uid: "x", kind: "proxmox", tags: &t }));
        assert!(matcher.accepts(&MatchContext { rule_uid: "x", kind: "snmp", tags: &t }));
    }

    #[test]
    fn le_type_d_equipement_et_la_regle_se_filtrent_aussi() {
        let matcher = ChannelMatcher {
            include: vec![
                Condition::Kind { value: "synology".to_string() },
                Condition::Rule { value: "disk_full".to_string() },
            ],
            exclude: Vec::new(),
        };
        let t = tags(&[]);
        assert!(matcher.accepts(&MatchContext {
            rule_uid: "disk_full",
            kind: "synology",
            tags: &t
        }));
        assert!(!matcher.accepts(&MatchContext {
            rule_uid: "cpu_high",
            kind: "synology",
            tags: &t
        }));
        assert!(!matcher.accepts(&MatchContext { rule_uid: "disk_full", kind: "snmp", tags: &t }));
    }

    #[test]
    fn l_apercu_par_equipement_dit_la_meme_chose_que_le_moteur() {
        // L'aperçu de l'interface n'évalue que la part « équipement » du filtre.
        // Dès que la condition de règle est satisfaite, les deux verdicts doivent
        // coïncider — sans quoi l'utilisateur voit une liste qui ment.
        let matcher = ChannelMatcher {
            include: vec![tag("site", "cellar"), Condition::Rule { value: "disk_full".into() }],
            exclude: vec![tag("role", "lab")],
        };
        let device = matcher.device_part();
        for tags_of in [
            tags(&[("site", "cellar"), ("role", "storage")]),
            tags(&[("site", "cellar"), ("role", "lab")]),
            tags(&[("site", "attic")]),
            tags(&[]),
        ] {
            let engine =
                matcher.accepts(&MatchContext { rule_uid: "disk_full", kind: "x", tags: &tags_of });
            let preview = device.accepts(&MatchContext { rule_uid: "", kind: "x", tags: &tags_of });
            assert_eq!(engine, preview, "preview and engine disagree on {tags_of:?}");
        }
        assert_eq!(matcher.rule_values(), (vec!["disk_full"], Vec::new()));
    }

    #[test]
    fn un_filtre_se_relit_tel_qu_il_a_ete_ecrit() {
        let matcher = ChannelMatcher {
            include: vec![tag("site", "cellar"), Condition::Kind { value: "snmp".to_string() }],
            exclude: vec![Condition::Rule { value: "noisy".to_string() }],
        };
        let json = serde_json::to_string(&matcher).expect("serialisable");
        assert!(json.contains("\"field\":\"tag\""), "{json}");
        let round: ChannelMatcher = serde_json::from_str(&json).expect("readable");
        assert_eq!(round, matcher);
        assert_eq!(round.len(), 3);
    }

    #[test]
    fn une_politique_enregistree_sans_filtre_se_relit() {
        // Exactement ce que contient la colonne `policy` d'un canal existant.
        let matcher: ChannelMatcher = serde_json::from_str("{}").expect("old rows must load");
        assert!(matcher.is_empty());
    }
}
