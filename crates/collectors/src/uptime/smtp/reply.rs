//! Analyse des réponses SMTP : codes, continuations, capacités annoncées.
//!
//! Module purement fonctionnel — pas une socket en vue — donc entièrement
//! vérifiable sans serveur de messagerie en face.

/// Réponse complète d'un serveur SMTP, continuations rassemblées.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Code à trois chiffres. `2xx` réussi, `4xx` temporaire, `5xx` définitif.
    pub code: u16,
    /// Lignes reçues, code et séparateur retirés.
    pub lines: Vec<String>,
}

impl Reply {
    pub fn is_positive(&self) -> bool {
        (200..400).contains(&self.code)
    }

    /// Vrai si le serveur a refusé les identifiants plutôt que la commande.
    ///
    /// `535` est le refus canonique ; `530` (authentification requise) et `534`
    /// (mécanisme trop faible) se corrigent au même endroit, dans le compte de
    /// supervision, et non sur le serveur.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self.code, 530 | 534 | 535 | 538)
    }

    /// Réponse mise en forme pour le journal, tronquée : un serveur bavard ne
    /// doit pas noyer le message d'échec.
    pub fn detail(&self) -> String {
        let text = self.lines.join(" / ");
        let text = if text.chars().count() > 160 {
            format!("{}…", text.chars().take(160).collect::<String>())
        } else {
            text
        };
        format!("{} {text}", self.code)
    }
}

/// Ce qui empêche de lire une réponse comme une réponse SMTP.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// La ligne ne commence pas par trois chiffres : en face, ce n'est pas SMTP.
    NotSmtp(String),
    /// Les continuations n'ont pas toutes le même code, ou n'en finissent pas.
    Malformed(String),
}

impl ReplyError {
    pub fn detail(&self) -> String {
        match self {
            Self::NotSmtp(line) => format!(
                "the service answered \"{}\", which is not an SMTP reply",
                truncate(line, 60)
            ),
            Self::Malformed(detail) => detail.clone(),
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// Une ligne de réponse : code, marqueur de continuation, texte.
///
/// Le quatrième caractère porte tout le sens : `-` annonce une suite, l'espace
/// clôt la réponse. C'est la seule façon de savoir qu'il faut relire.
#[derive(Debug)]
pub struct Line {
    pub code: u16,
    pub last: bool,
    pub text: String,
}

pub fn parse_line(raw: &str) -> Result<Line, ReplyError> {
    let bytes = raw.as_bytes();
    if bytes.len() < 3 || !bytes[..3].iter().all(u8::is_ascii_digit) {
        return Err(ReplyError::NotSmtp(raw.to_string()));
    }
    let code: u16 = raw[..3].parse().map_err(|_| ReplyError::NotSmtp(raw.to_string()))?;
    let (last, text) = match bytes.get(3) {
        None => (true, String::new()),
        Some(b' ') => (true, raw[4..].to_string()),
        Some(b'-') => (false, raw[4..].to_string()),
        Some(_) => return Err(ReplyError::NotSmtp(raw.to_string())),
    };
    Ok(Line { code, last, text })
}

/// Capacités annoncées par `EHLO`, en majuscules.
///
/// La première ligne d'une réponse à `EHLO` est le nom du serveur, pas une
/// capacité : la sauter évite d'annoncer « MAIL.EXEMPLE.FR » comme extension.
pub fn capabilities(reply: &Reply) -> Vec<String> {
    reply
        .lines
        .iter()
        .skip(1)
        .map(|line| line.trim().to_ascii_uppercase())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Vrai si la capacité est annoncée. `AUTH PLAIN LOGIN` répond oui à `AUTH`
/// comme à `AUTH PLAIN`, parce que l'utilisateur écrira l'une ou l'autre forme.
pub fn has_capability(capabilities: &[String], wanted: &str) -> bool {
    let wanted = wanted.trim().to_ascii_uppercase();
    if wanted.is_empty() {
        return true;
    }
    capabilities.iter().any(|capability| {
        capability == &wanted
            || capability.starts_with(&format!("{wanted} "))
            || capability.split_whitespace().collect::<Vec<_>>().join(" ").contains(&wanted)
    })
}

/// Mécanismes d'authentification annoncés dans la ligne `AUTH`.
pub fn auth_mechanisms(capabilities: &[String]) -> Vec<String> {
    capabilities
        .iter()
        .filter_map(|capability| {
            capability
                .strip_prefix("AUTH ")
                .or_else(|| capability.strip_prefix("AUTH="))
                .map(str::to_string)
        })
        .flat_map(|list| {
            list.split_whitespace().map(str::to_string).collect::<Vec<_>>().into_iter()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(code: u16, lines: &[&str]) -> Reply {
        Reply { code, lines: lines.iter().map(|l| (*l).to_string()).collect() }
    }

    #[test]
    fn une_ligne_simple_clot_la_reponse() {
        let line = parse_line("220 mail.exemple.fr ESMTP Postfix").unwrap();
        assert_eq!(line.code, 220);
        assert!(line.last);
        assert_eq!(line.text, "mail.exemple.fr ESMTP Postfix");
    }

    #[test]
    fn un_tiret_annonce_une_suite() {
        let line = parse_line("250-STARTTLS").unwrap();
        assert_eq!(line.code, 250);
        assert!(!line.last, "le tiret veut dire « relis »");
        assert_eq!(line.text, "STARTTLS");
    }

    /// Un service qui n'est pas un serveur SMTP doit se signaler comme tel, et
    /// non produire un code inventé : c'est la différence entre « corrigez le
    /// port » et « votre relais est en panne ».
    #[test]
    fn ce_qui_nest_pas_du_smtp_est_refuse_explicitement() {
        for brut in ["SSH-2.0-OpenSSH_9.6", "", "HTTP/1.1 200 OK", "25X hello"] {
            let error = parse_line(brut).unwrap_err();
            assert!(matches!(error, ReplyError::NotSmtp(_)), "« {brut} » : {error:?}");
            assert!(error.detail().contains("not an SMTP reply"));
        }
    }

    #[test]
    fn les_familles_de_codes_se_distinguent() {
        assert!(reply(250, &["OK"]).is_positive());
        assert!(reply(354, &["go ahead"]).is_positive());
        assert!(!reply(421, &["too busy"]).is_positive());
        assert!(!reply(550, &["no"]).is_positive());
    }

    #[test]
    fn un_refus_didentifiants_se_distingue_dun_refus_de_commande() {
        assert!(reply(535, &["5.7.8 Authentication credentials invalid"]).is_auth_failure());
        assert!(reply(530, &["5.7.0 Authentication required"]).is_auth_failure());
        assert!(!reply(502, &["command not implemented"]).is_auth_failure());
        assert!(!reply(235, &["ok"]).is_auth_failure());
    }

    #[test]
    fn la_premiere_ligne_dun_ehlo_est_le_nom_du_serveur_pas_une_capacite() {
        let ehlo = reply(250, &["mail.exemple.fr", "PIPELINING", "STARTTLS", "AUTH PLAIN LOGIN"]);
        let capabilities = capabilities(&ehlo);
        assert_eq!(capabilities, vec!["PIPELINING", "STARTTLS", "AUTH PLAIN LOGIN"]);
        assert!(!has_capability(&capabilities, "MAIL.EXEMPLE.FR"));
    }

    #[test]
    fn une_capacite_se_reconnait_avec_ou_sans_ses_arguments() {
        let ehlo = reply(250, &["mail", "STARTTLS", "AUTH PLAIN LOGIN", "SIZE 35882577"]);
        let capabilities = capabilities(&ehlo);
        assert!(has_capability(&capabilities, "STARTTLS"));
        assert!(has_capability(&capabilities, "starttls"), "la casse ne compte pas");
        assert!(has_capability(&capabilities, "AUTH"));
        assert!(has_capability(&capabilities, "SIZE"));
        assert!(!has_capability(&capabilities, "DSN"));
        assert!(has_capability(&capabilities, ""), "aucune attente vaut attente satisfaite");
    }

    #[test]
    fn les_mecanismes_dauthentification_se_lisent_dans_la_ligne_auth() {
        let ehlo = reply(250, &["mail", "AUTH PLAIN LOGIN CRAM-MD5"]);
        assert_eq!(auth_mechanisms(&capabilities(&ehlo)), vec!["PLAIN", "LOGIN", "CRAM-MD5"]);
        // Forme historique avec le signe égal, encore servie par de vieux relais.
        let vieux = reply(250, &["mail", "AUTH=LOGIN PLAIN"]);
        assert_eq!(auth_mechanisms(&capabilities(&vieux)), vec!["LOGIN", "PLAIN"]);
        assert!(auth_mechanisms(&capabilities(&reply(250, &["mail", "STARTTLS"]))).is_empty());
    }

    #[test]
    fn le_detail_dune_reponse_reste_court() {
        let long = "x".repeat(400);
        let detail = reply(550, &[&long]).detail();
        assert!(detail.starts_with("550 "));
        assert!(detail.chars().count() < 200, "{}", detail.len());
    }
}
