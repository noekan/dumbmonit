//! Vérifications appliquées à une réponse HTTP.
//!
//! Tout ce module est purement fonctionnel : il ne connaît ni socket ni client
//! HTTP, seulement un code de statut, un corps et une attente. C'est ce qui permet
//! de couvrir par des tests les cas tordus — un mot-clé à cheval sur deux lignes,
//! un `0` JSON qui n'est pas `false`, une plage de statuts inversée — sans avoir à
//! monter un serveur en face.

use std::ops::RangeInclusive;

use dumbmonit_proto::ProbeError;
use serde_json::Value;

/// Plages de codes de statut considérés comme normaux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRanges(Vec<RangeInclusive<u16>>);

impl Default for StatusRanges {
    /// Toute la famille 2xx : ce que l'on attend d'un service en bonne santé.
    // clippy soupçonne ici l'erreur classique du `vec![0..=9]` écrit pour obtenir
    // dix éléments. Ce n'en est pas une : le champ est bien une liste de plages,
    // dont celle-ci n'est que la première.
    #[allow(clippy::single_range_in_vec_init)]
    fn default() -> Self {
        Self(vec![200..=299])
    }
}

impl StatusRanges {
    /// Analyse une spécification `200-299,301,404`.
    ///
    /// Accepter des codes hors 2xx n'est pas un caprice : une page protégée
    /// répond 401 quand tout va bien, un endpoint de santé peut répondre 204, et
    /// un service correctement configuré derrière un portail répond 302.
    pub fn parse(spec: &str) -> Result<Self, ProbeError> {
        let mut ranges = Vec::new();
        for item in spec.split(',').map(str::trim).filter(|item| !item.is_empty()) {
            ranges.push(parse_range(item)?);
        }
        if ranges.is_empty() {
            return Err(ProbeError::Config(
                "\"accepted_status\" names no code; write for example \"200-299\" or \
                 \"200,204,301\""
                    .to_string(),
            ));
        }
        Ok(Self(ranges))
    }

    pub fn accepts(&self, code: u16) -> bool {
        self.0.iter().any(|range| range.contains(&code))
    }

    /// Forme lisible, pour le message d'échec.
    pub fn describe(&self) -> String {
        self.0
            .iter()
            .map(|range| {
                if range.start() == range.end() {
                    range.start().to_string()
                } else {
                    format!("{}-{}", range.start(), range.end())
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn parse_range(item: &str) -> Result<RangeInclusive<u16>, ProbeError> {
    let invalid = || {
        ProbeError::Config(format!(
            "invalid status code in \"accepted_status\": \"{item}\" (expected: a code \
             between 100 and 599, or a range \"200-299\")"
        ))
    };

    // Le tiret est cherché à partir du deuxième caractère, et par `char_indices`
    // plutôt que par un découpage d'octets : une saisie accentuée ne doit pas faire
    // paniquer la lecture de la configuration.
    let separator = item.char_indices().skip(1).find(|(_, c)| *c == '-').map(|(index, _)| index);
    let (start, end) = match separator {
        Some(index) => (&item[..index], &item[index + 1..]),
        None => (item, item),
    };

    let start: u16 = start.trim().parse().map_err(|_| invalid())?;
    let end: u16 = end.trim().parse().map_err(|_| invalid())?;
    if !(100..=599).contains(&start) || !(100..=599).contains(&end) || start > end {
        return Err(invalid());
    }
    Ok(start..=end)
}

/// Recherche d'un mot-clé dans le corps de la réponse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyword {
    pub needle: String,
    /// Vrai si le mot-clé doit être **absent**. Utile pour détecter une page
    /// d'erreur qui répond pourtant 200 : « Service Unavailable », « Exception ».
    pub inverted: bool,
    pub case_sensitive: bool,
}

impl Keyword {
    /// `None` si l'attente est satisfaite, sinon le motif de l'échec.
    pub fn verify(&self, body: &str) -> Option<String> {
        let present = if self.case_sensitive {
            body.contains(&self.needle)
        } else {
            body.to_lowercase().contains(&self.needle.to_lowercase())
        };

        match (present, self.inverted) {
            (true, false) | (false, true) => None,
            (false, false) => {
                Some(format!("keyword \"{}\" missing from the response", self.needle))
            }
            (true, true) => Some(format!("forbidden keyword \"{}\" present", self.needle)),
        }
    }
}

/// Un pas dans un chemin JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

/// Extrait une valeur d'un document JSON par son chemin.
///
/// Syntaxe volontairement réduite à ce qui sert : `$.etat`, `etat.base`,
/// `services[0].sain`. Pas de filtre ni de joker — un moniteur qui demanderait
/// d'apprendre JSONPath ne serait pas utilisé.
pub fn json_at<'a>(document: &'a Value, path: &str) -> Result<&'a Value, String> {
    let steps = parse_path(path)?;
    let mut current = document;
    for (position, step) in steps.iter().enumerate() {
        current = match step {
            Step::Key(key) => current.get(key).ok_or_else(|| {
                format!("path \"{path}\": key \"{key}\" missing{}", context(&steps, position))
            })?,
            Step::Index(index) => current.get(index).ok_or_else(|| {
                format!(
                    "path \"{path}\": index {index} out of the array{}",
                    context(&steps, position)
                )
            })?,
        };
    }
    Ok(current)
}

/// Rappelle où l'on en était, pour que le message serve à corriger le chemin.
fn context(steps: &[Step], position: usize) -> String {
    if position == 0 {
        return String::new();
    }
    let parcouru: Vec<String> = steps[..position]
        .iter()
        .map(|step| match step {
            Step::Key(key) => key.clone(),
            Step::Index(index) => format!("[{index}]"),
        })
        .collect();
    format!(" (after \"{}\")", parcouru.join("."))
}

fn parse_path(path: &str) -> Result<Vec<Step>, String> {
    let trimmed = path.trim().trim_start_matches("$.").trim_start_matches('$');
    let mut steps = Vec::new();

    for segment in trimmed.split('.').filter(|segment| !segment.is_empty()) {
        let (name, rest) = match segment.find('[') {
            Some(index) => segment.split_at(index),
            None => (segment, ""),
        };
        if !name.is_empty() {
            steps.push(Step::Key(name.to_string()));
        }

        let mut rest = rest;
        while !rest.is_empty() {
            let close =
                rest.find(']').ok_or_else(|| format!("path \"{path}\": unclosed bracket"))?;
            let index: usize = rest[1..close]
                .trim()
                .parse()
                .map_err(|_| format!("path \"{path}\": invalid array index"))?;
            steps.push(Step::Index(index));
            rest = &rest[close + 1..];
        }
    }

    if steps.is_empty() {
        return Err(format!("empty JSON path: \"{path}\""));
    }
    Ok(steps)
}

/// Compare une valeur JSON à l'attente exprimée par l'utilisateur.
///
/// L'attente est toujours du texte — elle vient d'une étiquette. La comparaison est
/// donc faite dans le type de la valeur trouvée : `ok` reste `ok`, `1` vaut `1.0`,
/// et `true` ne vaut pas `1`. Sans cela, un `"status": 200` obligerait à deviner si
/// c'est « 200 » ou « 200.0 » qu'il faut écrire.
pub fn value_matches(value: &Value, expected: &str) -> bool {
    let expected = expected.trim();
    match value {
        Value::String(text) => text == expected,
        Value::Number(number) => match expected.parse::<f64>() {
            Ok(wanted) => {
                number.as_f64().is_some_and(|found| (found - wanted).abs() < f64::EPSILON)
            }
            Err(_) => false,
        },
        Value::Bool(flag) => matches!(
            (flag, expected.to_ascii_lowercase().as_str()),
            (true, "true") | (false, "false")
        ),
        Value::Null => expected.eq_ignore_ascii_case("null"),
        // Tableaux et objets : l'attente est relue comme du JSON, ce qui rend la
        // comparaison insensible aux espaces de mise en forme.
        other => serde_json::from_str::<Value>(expected).is_ok_and(|wanted| &wanted == other),
    }
}

/// Forme lisible d'une valeur JSON, pour le message d'échec.
pub fn value_label(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        serde_json::from_str(text).expect("JSON de test valide")
    }

    #[test]
    fn les_codes_acceptes_par_defaut_sont_la_famille_2xx() {
        let ranges = StatusRanges::default();
        for code in [200, 201, 204, 299] {
            assert!(ranges.accepts(code), "{code} devrait être accepté");
        }
        for code in [199, 300, 301, 404, 500] {
            assert!(!ranges.accepts(code), "{code} ne devrait pas l'être");
        }
    }

    #[test]
    fn les_plages_melangent_codes_isoles_et_intervalles() {
        let ranges = StatusRanges::parse("200-204, 301 ,404").unwrap();
        assert!(ranges.accepts(200));
        assert!(ranges.accepts(204));
        assert!(!ranges.accepts(205));
        assert!(ranges.accepts(301));
        assert!(ranges.accepts(404));
        assert!(!ranges.accepts(500));
        assert_eq!(ranges.describe(), "200-204, 301, 404");
    }

    #[test]
    fn une_plage_absurde_est_refusee_a_la_configuration() {
        for spec in ["", "  ,  ", "abc", "200-", "-299", "299-200", "99", "600", "200-600"] {
            let error = StatusRanges::parse(spec);
            assert!(error.is_err(), "« {spec} » aurait dû être refusée");
            assert!(matches!(error.unwrap_err(), ProbeError::Config(_)));
        }
    }

    #[test]
    fn un_code_unique_se_decrit_sans_tiret() {
        assert_eq!(StatusRanges::parse("204").unwrap().describe(), "204");
    }

    #[test]
    fn le_mot_cle_est_cherche_sans_tenir_compte_de_la_casse_par_defaut() {
        let keyword =
            Keyword { needle: "Bienvenue".into(), inverted: false, case_sensitive: false };
        assert_eq!(keyword.verify("<h1>BIENVENUE chez vous</h1>"), None);
        assert!(keyword.verify("<h1>Erreur 500</h1>").is_some());
    }

    #[test]
    fn la_sensibilite_a_la_casse_sactive_a_la_demande() {
        let keyword = Keyword { needle: "OK".into(), inverted: false, case_sensitive: true };
        assert_eq!(keyword.verify("statut : OK"), None);
        assert!(keyword.verify("statut : ok").is_some(), "« ok » n'est pas « OK »");
    }

    /// Le cas qui justifie l'option : un serveur applicatif qui rend une page
    /// d'erreur avec un code 200 tout à fait honorable.
    #[test]
    fn un_mot_cle_interdit_detecte_une_page_derreur_qui_repond_200() {
        let keyword = Keyword { needle: "Exception".into(), inverted: true, case_sensitive: false };
        assert_eq!(keyword.verify("<h1>Tableau de bord</h1>"), None);
        let echec = keyword.verify("java.lang.NullPointerException").expect("échec attendu");
        assert!(echec.contains("forbidden"), "{echec}");
    }

    #[test]
    fn le_mot_cle_traverse_les_retours_a_la_ligne() {
        let keyword = Keyword { needle: "en ligne".into(), inverted: false, case_sensitive: false };
        assert_eq!(keyword.verify("statut\n  : en ligne\n"), None);
    }

    #[test]
    fn le_chemin_json_descend_les_objets_et_les_tableaux() {
        let document = json(r#"{"etat":{"base":"ok"},"services":[{"sain":true},{"sain":false}]}"#);
        assert_eq!(json_at(&document, "etat.base").unwrap(), &json(r#""ok""#));
        assert_eq!(json_at(&document, "$.etat.base").unwrap(), &json(r#""ok""#));
        assert_eq!(json_at(&document, "services[1].sain").unwrap(), &Value::Bool(false));
    }

    #[test]
    fn un_chemin_absent_dit_ou_la_descente_sest_arretee() {
        let document = json(r#"{"etat":{"base":"ok"}}"#);
        let error = json_at(&document, "etat.cache").unwrap_err();
        assert!(error.contains("cache"), "{error}");
        assert!(error.contains("after \"etat\""), "{error}");

        assert!(json_at(&document, "services[0]").is_err());
        assert!(json_at(&document, "etat[0]").is_err(), "un objet n'est pas un tableau");
    }

    #[test]
    fn un_chemin_mal_ecrit_est_signale_sans_panique() {
        let document = json(r#"{"a":[1,2]}"#);
        assert!(json_at(&document, "").is_err());
        assert!(json_at(&document, "$").is_err());
        assert!(json_at(&document, "a[0").is_err());
        assert!(json_at(&document, "a[x]").is_err());
    }

    #[test]
    fn la_comparaison_respecte_le_type_de_la_valeur_trouvee() {
        assert!(value_matches(&json(r#""ok""#), "ok"));
        assert!(!value_matches(&json(r#""ok""#), "OK"));

        assert!(value_matches(&json("200"), "200"));
        assert!(value_matches(&json("1.5"), "1.5"));
        assert!(!value_matches(&json("200"), "201"));

        assert!(value_matches(&json("true"), "true"));
        assert!(value_matches(&json("true"), "TRUE"));
        // Le piège classique : en JSON, 1 n'est pas true.
        assert!(!value_matches(&json("true"), "1"));
        assert!(!value_matches(&json("1"), "true"));

        assert!(value_matches(&Value::Null, "null"));
        assert!(value_matches(&json(r#"[1,2]"#), "[1,2]"));
        assert!(value_matches(&json(r#"[1,2]"#), "[1, 2]"), "les espaces de mise en forme");
        assert!(!value_matches(&json(r#"[1,2]"#), "[1,3]"));
    }

    #[test]
    fn la_valeur_est_affichee_sans_guillemets_superflus() {
        assert_eq!(value_label(&json(r#""degrade""#)), "degrade");
        assert_eq!(value_label(&json("42")), "42");
    }
}
