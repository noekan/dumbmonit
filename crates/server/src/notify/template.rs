//! Gabarits du canal personnalisé.
//!
//! Un service tiers sur trois attend une charge utile qu'aucun canal générique ne
//! produira jamais. Plutôt que d'ajouter un notificateur par service exotique, on
//! laisse l'utilisateur écrire lui-même le corps de la requête, avec des variables
//! substituées au moment de l'envoi.
//!
//! Deux décisions structurent le module. D'abord la syntaxe `{{variable}}` : un
//! gabarit JSON est truffé d'accolades simples, et les confondre avec des variables
//! rendrait la fonctionnalité inutilisable. Ensuite la validation à
//! l'enregistrement : un gabarit fautif doit être refusé pendant que l'utilisateur
//! regarde son formulaire, et non produire un message vide la nuit où l'incident
//! arrive.

use std::collections::BTreeMap;

use crate::notify::error::NotifyError;
use crate::notify::http::percent_encode;
use crate::notify::message::TEMPLATE_VARIABLES;

/// Encodage appliqué aux valeurs substituées — jamais au texte du gabarit, qui est
/// écrit par l'utilisateur et doit rester intact.
///
/// C'est le point qui fait la différence entre un canal fiable et un canal qui
/// tombe le jour où une règle s'appelle « Disque "système" plein ».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escaping {
    /// Échappe guillemets, contre-obliques et retours à la ligne : la valeur peut
    /// être posée entre guillemets dans un corps JSON.
    Json,
    /// Encodage pourcent, pour un corps de formulaire ou une chaîne de requête.
    Form,
    /// Aucune transformation, pour du texte brut.
    None,
}

impl Escaping {
    fn apply(self, value: &str) -> String {
        match self {
            // Passer par serde plutôt que par une table écrite à la main : les
            // caractères de contrôle et les paires de substitution sont exactement
            // là où l'on se trompe.
            Self::Json => {
                let quoted = serde_json::Value::String(value.to_string()).to_string();
                quoted[1..quoted.len() - 1].to_string()
            }
            Self::Form => percent_encode(value),
            Self::None => value.to_string(),
        }
    }
}

/// Fragment d'un gabarit analysé.
#[derive(Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Variable(String),
}

/// Gabarit analysé et validé, prêt à être rendu autant de fois que nécessaire.
#[derive(Debug, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
}

impl Template {
    /// Analyse un gabarit et vérifie que chaque variable citée existe.
    ///
    /// `field` nomme le réglage fautif dans le message d'erreur : l'utilisateur doit
    /// savoir si le problème vient de l'URL ou du corps.
    pub fn parse(source: &str, field: &str) -> Result<Self, NotifyError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut rest = source;

        while let Some(start) = rest.find("{{") {
            literal.push_str(&rest[..start]);
            let after = &rest[start + 2..];

            let Some(end) = after.find("}}") else {
                return Err(NotifyError::Config(format!(
                    "\"{field}\": unclosed \"{{{{ }}}}\" braces; every variable must end \
                     with \"}}}}\""
                )));
            };

            let name = after[..end].trim().to_string();
            if name.is_empty() {
                return Err(NotifyError::Config(format!(
                    "\"{field}\": an empty variable \"{{{{}}}}\" means nothing"
                )));
            }
            if !is_known(&name) {
                return Err(NotifyError::Config(format!(
                    "\"{field}\": unknown variable \"{name}\" (available: {})",
                    known_names().join(", ")
                )));
            }

            if !literal.is_empty() {
                segments.push(Segment::Literal(std::mem::take(&mut literal)));
            }
            segments.push(Segment::Variable(name));
            rest = &after[end + 2..];
        }

        literal.push_str(rest);
        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }
        Ok(Self { segments })
    }

    /// Substitue les variables. Une variable absente de la table rend une chaîne
    /// vide : elle a déjà été validée à l'analyse, son absence ici ne peut venir que
    /// d'un canal qui n'a rien à mettre dedans, comme un lien non configuré.
    pub fn render(&self, variables: &BTreeMap<&'static str, String>, escaping: Escaping) -> String {
        let mut rendered = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => rendered.push_str(text),
                Segment::Variable(name) => {
                    let value = variables.get(name.as_str()).map_or("", String::as_str);
                    rendered.push_str(&escaping.apply(value));
                }
            }
        }
        rendered
    }

    /// Vrai si le gabarit cite cette variable, pour prévenir l'utilisateur qu'il
    /// lui manque le secret correspondant.
    pub fn uses(&self, name: &str) -> bool {
        self.segments.iter().any(|segment| matches!(segment, Segment::Variable(v) if v == name))
    }
}

fn is_known(name: &str) -> bool {
    TEMPLATE_VARIABLES.iter().any(|(known, _)| *known == name)
}

fn known_names() -> Vec<&'static str> {
    TEMPLATE_VARIABLES.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables() -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            ("title", "nas — Disk full".to_string()),
            ("target", "nas".to_string()),
            ("severity", "warning".to_string()),
            ("value_raw", "95".to_string()),
            ("message", "ligne 1\nligne 2".to_string()),
            ("rule", r#"Disque "système" plein"#.to_string()),
            ("link", String::new()),
        ])
    }

    fn rendu(gabarit: &str, escaping: Escaping) -> String {
        Template::parse(gabarit, "body_template")
            .expect("valid template")
            .render(&variables(), escaping)
    }

    #[test]
    fn un_gabarit_sans_variable_est_recopie_tel_quel() {
        assert_eq!(rendu("hello", Escaping::Json), "hello");
        assert_eq!(rendu("", Escaping::Json), "");
    }

    #[test]
    fn les_variables_sont_substituees() {
        assert_eq!(rendu("{{target}} : {{severity}}", Escaping::None), "nas : warning");
    }

    #[test]
    fn les_espaces_autour_du_nom_sont_toleres() {
        // Le formulaire est saisi à la main : « {{ target }} » est la faute la plus
        // probable, et la refuser n'apporterait rien.
        assert_eq!(rendu("{{ target }}", Escaping::None), "nas");
    }

    #[test]
    fn les_accolades_simples_d_un_corps_json_restent_litterales() {
        let gabarit = r#"{"a": {"b": "{{target}}"}}"#;
        assert_eq!(rendu(gabarit, Escaping::Json), r#"{"a": {"b": "nas"}}"#);
    }

    #[test]
    fn une_valeur_a_guillemets_ne_casse_pas_le_json_produit() {
        let produit = rendu(r#"{"rule": "{{rule}}"}"#, Escaping::Json);
        assert_eq!(produit, r#"{"rule": "Disque \"système\" plein"}"#);
        // Le contrat, c'est que le résultat soit du JSON valide.
        let valeur: serde_json::Value = serde_json::from_str(&produit).expect("JSON valide");
        assert_eq!(valeur["rule"], r#"Disque "système" plein"#);
    }

    #[test]
    fn un_retour_a_la_ligne_est_echappe_en_json() {
        let produit = rendu(r#"{"m": "{{message}}"}"#, Escaping::Json);
        assert_eq!(produit, r#"{"m": "ligne 1\nligne 2"}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&produit).is_ok());
    }

    #[test]
    fn l_encodage_formulaire_protege_les_separateurs() {
        let produit = rendu("text={{title}}&level={{severity}}", Escaping::Form);
        assert_eq!(produit, "text=nas%20%E2%80%94%20Disk%20full&level=warning");
    }

    #[test]
    fn une_valeur_numerique_reste_utilisable_comme_nombre_json() {
        let produit = rendu(r#"{"v": {{value_raw}}}"#, Escaping::Json);
        assert_eq!(produit, r#"{"v": 95}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&produit).is_ok());
    }

    #[test]
    fn une_variable_inconnue_est_refusee_avec_la_liste_des_variables_valides() {
        let Err(erreur) = Template::parse("{{severite}}", "body_template") else {
            panic!("an unknown variable should be rejected")
        };
        let texte = erreur.to_string();
        assert!(texte.contains("severite"), "{texte}");
        assert!(texte.contains("body_template"), "{texte}");
        assert!(texte.contains("severity"), "the list of valid variables helps fixing the typo");
        assert!(!erreur.is_transient(), "retrying will not fix a typo");
    }

    #[test]
    fn une_accolade_non_fermee_est_refusee() {
        let Err(erreur) = Template::parse(r#"{"t": "{{title"}"#, "body_template") else {
            panic!("unclosed braces should be rejected")
        };
        assert!(erreur.to_string().contains("unclosed"), "{erreur}");
    }

    #[test]
    fn une_variable_vide_est_refusee() {
        assert!(Template::parse("{{}}", "body_template").is_err());
        assert!(Template::parse("{{   }}", "body_template").is_err());
    }

    #[test]
    fn une_accolade_fermante_isolee_reste_litterale() {
        // « }} » sans « {{ » devant clôt un objet JSON imbriqué : le refuser
        // interdirait la moitié des gabarits réalistes.
        assert_eq!(rendu(r#"{"a":{"b":1}}"#, Escaping::Json), r#"{"a":{"b":1}}"#);
    }

    #[test]
    fn une_variable_connue_mais_absente_de_la_table_rend_du_vide() {
        // « count » est documentée ; un canal qui ne la fournit pas ne doit pas
        // produire « {{count}} » en clair dans le message envoyé.
        assert_eq!(rendu("n={{count}}", Escaping::None), "n=");
    }

    #[test]
    fn le_gabarit_sait_dire_quelles_variables_il_cite() {
        let gabarit = Template::parse("{{token}} {{title}}", "body_template").unwrap();
        assert!(gabarit.uses("token"));
        assert!(!gabarit.uses("severity"));
    }

    #[test]
    fn plusieurs_occurrences_de_la_meme_variable_sont_toutes_substituees() {
        assert_eq!(rendu("{{target}}/{{target}}", Escaping::None), "nas/nas");
    }
}
