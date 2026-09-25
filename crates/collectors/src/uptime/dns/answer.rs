//! Analyse des réponses DNS : types d'enregistrement, normalisation, vérification.
//!
//! Module purement fonctionnel : il ne connaît ni résolveur ni socket, ce qui rend
//! toute sa logique vérifiable sans serveur DNS en face.

use dumbmonit_proto::ProbeError;
use hickory_resolver::proto::rr::{Record, RecordType};

/// Types d'enregistrement proposés.
///
/// La liste est volontairement fermée plutôt que déléguée à `RecordType::from_str` :
/// ce dernier accepte des types de transfert de zone ou de signature qui n'ont
/// aucun sens dans un moniteur, et une faute de frappe doit se voir comme une
/// erreur de configuration, pas produire une requête bizarre.
const SUPPORTED: &[(&str, RecordType)] = &[
    ("A", RecordType::A),
    ("AAAA", RecordType::AAAA),
    ("CAA", RecordType::CAA),
    ("CNAME", RecordType::CNAME),
    ("MX", RecordType::MX),
    ("NS", RecordType::NS),
    ("PTR", RecordType::PTR),
    ("SOA", RecordType::SOA),
    ("SRV", RecordType::SRV),
    ("TXT", RecordType::TXT),
];

/// Traduit le libellé saisi par l'utilisateur en type d'enregistrement.
pub fn parse_record_type(raw: &str) -> Result<RecordType, ProbeError> {
    let wanted = raw.trim().to_ascii_uppercase();
    SUPPORTED
        .iter()
        .find(|(label, _)| *label == wanted)
        .map(|(_, record_type)| *record_type)
        .ok_or_else(|| {
            ProbeError::Config(format!(
                "unknown record type \"{raw}\"; accepted values: {}",
                SUPPORTED.iter().map(|(label, _)| *label).collect::<Vec<_>>().join(", ")
            ))
        })
}

/// Met une valeur sous une forme comparable.
///
/// Le DNS n'est pas sensible à la casse, écrit ses noms avec un point final et
/// entoure le texte de guillemets. Comparer les chaînes brutes ferait échouer une
/// attente pourtant satisfaite, ce qui est le pire des faux positifs : une alerte
/// de détournement de zone sur une zone parfaitement saine.
pub fn normalize(value: &str) -> String {
    value.replace('"', "").trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Valeurs des enregistrements de la réponse, normalisées.
pub fn render(records: &[Record]) -> Vec<String> {
    records.iter().map(|record| normalize(&record.data.to_string())).collect()
}

/// Façon de confronter la réponse aux valeurs attendues.
///
/// `Contains` est le réglage courant : la valeur attendue doit se retrouver
/// quelque part dans un enregistrement. `Exact` est celui d'une zone que l'on
/// tient : la réponse ne doit contenir *ni plus ni moins* que ce qui est écrit,
/// et un enregistrement ajouté à côté du bon — le tour de passe-passe d'une zone
/// détournée — fait alors échouer la sonde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Contains,
    Exact,
}

impl Mode {
    pub fn parse(raw: &str) -> Result<Self, ProbeError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "contains" | "" => Ok(Self::Contains),
            "exact" => Ok(Self::Exact),
            other => Err(ProbeError::Config(format!(
                "\"expect_mode\" expects \"contains\" or \"exact\", got \"{other}\""
            ))),
        }
    }
}

/// Valeurs de la réponse qu'aucune attente ne couvre, en mode `Exact`.
///
/// C'est la moitié manquante de [`missing`] : une zone détournée n'efface pas
/// toujours le bon enregistrement, elle en ajoute souvent un à côté, et seul un
/// contrôle dans ce sens-là le voit.
pub fn unexpected(answers: &[String], expected: &[String]) -> Vec<String> {
    let expected: Vec<String> = expected.iter().map(|value| normalize(value)).collect();
    answers
        .iter()
        .filter(|answer| {
            !expected.iter().any(|wanted| *answer == wanted || answer.contains(wanted))
        })
        .cloned()
        .collect()
}

/// Valeurs interdites présentes dans la réponse.
///
/// L'usage : un enregistrement `A` que l'on a supprimé et qui ne doit jamais
/// revenir, une adresse de l'ancien hébergeur, un `TXT` de validation périmé.
pub fn forbidden_present(answers: &[String], forbidden: &[String]) -> Vec<String> {
    forbidden
        .iter()
        .filter(|banned| {
            let banned = normalize(banned);
            answers.iter().any(|answer| answer == &banned || answer.contains(&banned))
        })
        .cloned()
        .collect()
}

/// Valeurs attendues absentes de la réponse.
///
/// L'inclusion vaut correspondance : un `MX` se lit `10 mail.exemple.fr`, un `TXT`
/// SPF fait deux cents caractères. Exiger l'égalité stricte obligerait à recopier
/// la priorité ou l'enregistrement entier, ce que personne ne fait correctement.
pub fn missing(answers: &[String], expected: &[String]) -> Vec<String> {
    expected
        .iter()
        .filter(|wanted| {
            let wanted = normalize(wanted);
            !answers.iter().any(|answer| answer == &wanted || answer.contains(&wanted))
        })
        .cloned()
        .collect()
}

/// Vrai si l'enregistrement porte bien le type demandé.
///
/// Un résolveur suit les `CNAME` : demander un `A` sur un alias ramène le `CNAME`
/// *et* le `A` final. Compter les deux gonflerait `probe_dns_answer_records` sans
/// que rien ne l'explique.
pub fn has_type(record: &Record, record_type: RecordType) -> bool {
    record.record_type() == record_type
}

/// Nombre d'enregistrements du type demandé.
pub fn count_of_type(records: &[Record], record_type: RecordType) -> usize {
    records.iter().filter(|record| has_type(record, record_type)).count()
}

/// Résultat vide : le nom existe peut-être, mais pas pour ce type.
pub fn is_empty_answer(records: &[Record], record_type: RecordType) -> bool {
    count_of_type(records, record_type) == 0
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    use hickory_resolver::proto::rr::rdata::{A, AAAA, CNAME, MX, TXT};
    use hickory_resolver::proto::rr::{Name, RData, Record};

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("nom de test valide")
    }

    pub fn a(value: &str, ip: [u8; 4]) -> Record {
        Record::from_rdata(name(value), 300, RData::A(A(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]))))
    }

    pub fn aaaa(value: &str, ip: Ipv6Addr) -> Record {
        Record::from_rdata(name(value), 300, RData::AAAA(AAAA(ip)))
    }

    pub fn cname(value: &str, target: &str) -> Record {
        Record::from_rdata(name(value), 300, RData::CNAME(CNAME(name(target))))
    }

    pub fn mx(value: &str, priority: u16, target: &str) -> Record {
        Record::from_rdata(name(value), 300, RData::MX(MX::new(priority, name(target))))
    }

    pub fn txt(value: &str, contents: &str) -> Record {
        Record::from_rdata(name(value), 300, RData::TXT(TXT::new(vec![contents.to_string()])))
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::fixtures::{a, aaaa, cname, mx, txt};
    use super::*;

    #[test]
    fn les_types_courants_sont_reconnus_quelle_que_soit_la_casse() {
        assert_eq!(parse_record_type("a").unwrap(), RecordType::A);
        assert_eq!(parse_record_type("AAAA").unwrap(), RecordType::AAAA);
        assert_eq!(parse_record_type(" cname ").unwrap(), RecordType::CNAME);
        assert_eq!(parse_record_type("Mx").unwrap(), RecordType::MX);
        assert_eq!(parse_record_type("TXT").unwrap(), RecordType::TXT);
        assert_eq!(parse_record_type("ns").unwrap(), RecordType::NS);
    }

    #[test]
    fn un_type_inconnu_liste_les_valeurs_acceptees() {
        let error = parse_record_type("AXFR").unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(error.to_string().contains("CNAME"), "{error}");
        assert!(!error.means_down());
    }

    #[test]
    fn la_normalisation_neutralise_casse_point_final_et_guillemets() {
        assert_eq!(normalize("Mail.Exemple.FR."), "mail.exemple.fr");
        assert_eq!(normalize("\"v=spf1 -all\""), "v=spf1 -all");
        assert_eq!(normalize("  1.2.3.4  "), "1.2.3.4");
    }

    #[test]
    fn les_valeurs_des_enregistrements_sont_rendues_comparables() {
        let records =
            [a("exemple.fr.", [203, 0, 113, 10]), cname("www.exemple.fr.", "exemple.fr.")];
        assert_eq!(render(&records), vec!["203.0.113.10", "exemple.fr"]);

        let six = aaaa("exemple.fr.", Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        assert_eq!(render(&[six]), vec!["2001:db8::1"]);
    }

    #[test]
    fn une_valeur_attendue_presente_ne_manque_pas() {
        let answers = render(&[a("exemple.fr.", [203, 0, 113, 10])]);
        assert!(missing(&answers, &["203.0.113.10".to_string()]).is_empty());
        // La casse et le point final de l'attente ne doivent pas la faire échouer.
        let alias = render(&[cname("www.exemple.fr.", "exemple.fr.")]);
        assert!(missing(&alias, &["Exemple.FR.".to_string()]).is_empty());
    }

    #[test]
    fn un_detournement_de_zone_se_voit_dans_les_valeurs_manquantes() {
        let answers = render(&[a("exemple.fr.", [198, 51, 100, 7])]);
        let manquantes = missing(&answers, &["203.0.113.10".to_string()]);
        assert_eq!(manquantes, vec!["203.0.113.10"]);
    }

    #[test]
    fn linclusion_suffit_pour_les_enregistrements_verbeux() {
        let courriel = render(&[mx("exemple.fr.", 10, "mail.exemple.fr.")]);
        assert!(
            missing(&courriel, &["mail.exemple.fr".to_string()]).is_empty(),
            "la priorité n'a pas à être recopiée"
        );

        let spf = render(&[txt("exemple.fr.", "v=spf1 include:_spf.exemple.fr -all")]);
        assert!(missing(&spf, &["include:_spf.exemple.fr".to_string()]).is_empty());
    }

    #[test]
    fn le_mode_exact_refuse_un_enregistrement_ajoute_a_cote_du_bon() {
        // Le détournement le plus discret : le bon « A » est toujours là, un
        // second le double. Le mode par défaut ne voit rien, le mode exact si.
        let answers =
            render(&[a("exemple.fr.", [203, 0, 113, 10]), a("exemple.fr.", [198, 51, 100, 7])]);
        let attendu = ["203.0.113.10".to_string()];
        assert!(missing(&answers, &attendu).is_empty(), "la valeur attendue est bien là");
        assert_eq!(unexpected(&answers, &attendu), vec!["198.51.100.7"]);
    }

    #[test]
    fn le_mode_exact_accepte_une_reponse_conforme_dans_nimporte_quel_ordre() {
        let answers =
            render(&[a("exemple.fr.", [203, 0, 113, 11]), a("exemple.fr.", [203, 0, 113, 10])]);
        let attendu = ["203.0.113.10".to_string(), "203.0.113.11".to_string()];
        assert!(missing(&answers, &attendu).is_empty());
        assert!(unexpected(&answers, &attendu).is_empty(), "l'ordre du DNS n'est pas stable");
    }

    #[test]
    fn une_valeur_interdite_se_signale_meme_quand_lattendue_est_la() {
        let answers = render(&[
            mx("exemple.fr.", 10, "mail.exemple.fr."),
            mx("exemple.fr.", 20, "vieux.hebergeur.net."),
        ]);
        assert!(missing(&answers, &["mail.exemple.fr".to_string()]).is_empty());
        assert_eq!(
            forbidden_present(&answers, &["vieux.hebergeur.net".to_string()]),
            vec!["vieux.hebergeur.net"]
        );
        assert!(forbidden_present(&answers, &["autre.net".to_string()]).is_empty());
    }

    #[test]
    fn les_modes_de_comparaison_se_lisent_et_se_refusent() {
        assert_eq!(Mode::parse("contains").unwrap(), Mode::Contains);
        assert_eq!(Mode::parse(" EXACT ").unwrap(), Mode::Exact);
        assert_eq!(Mode::parse("").unwrap(), Mode::Contains);
        let error = Mode::parse("presque").unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "une faute de frappe n'est pas une zone détournée");
    }

    #[test]
    fn les_enregistrements_intermediaires_ne_sont_pas_comptes() {
        let records = [
            cname("www.exemple.fr.", "exemple.fr."),
            a("exemple.fr.", [203, 0, 113, 10]),
            a("exemple.fr.", [203, 0, 113, 11]),
        ];
        assert_eq!(count_of_type(&records, RecordType::A), 2);
        assert_eq!(count_of_type(&records, RecordType::CNAME), 1);
        assert!(!is_empty_answer(&records, RecordType::A));
        assert!(is_empty_answer(&records, RecordType::MX), "aucun MX dans cette réponse");
    }
}
