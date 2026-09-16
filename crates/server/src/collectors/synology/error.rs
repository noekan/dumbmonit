//! Traduction des codes d'erreur de l'API web de DSM.
//!
//! Particularité de Synology, structurante pour tout ce collecteur : **une erreur
//! applicative arrive en HTTP 200**. Le statut HTTP ne dit rien ; c'est le corps
//! JSON qui porte `{"success": false, "error": {"code": 403}}`. Se fier au statut
//! reviendrait à considérer un mot de passe refusé comme une collecte réussie.
//!
//! Deux familles de codes coexistent, et le même nombre n'a pas le même sens dans
//! les deux :
//!
//! * les **codes communs** (100 à 150), valables pour toutes les API ;
//! * les **codes propres à `SYNO.API.Auth`** (400 à 410), qui ne sont émis que par
//!   la requête de connexion. Le 403 y signifie « code de vérification en deux
//!   étapes requis », alors qu'il n'a rien à voir avec le 403 de HTTP.
//!
//! D'où deux points d'entrée distincts : [`from_auth_code`] et [`from_api_code`].
//! Ce module est purement fonctionnel, donc entièrement testable sans NAS.
//!
//! Source : « DSM Login Web API Guide », Synology, révision du 19 avril 2023,
//! chapitres « Common Error Codes » et « SYNO.API.Auth › API Error Codes ».

use dumbmonit_proto::ProbeError;

/// Codes qui signalent une session périmée plutôt qu'un refus définitif.
///
/// Ce sont les seuls qui justifient de refaire une connexion : le `sid` a expiré,
/// a été invalidé, ou l'utilisateur s'est reconnecté ailleurs. Tous les autres
/// codes d'authentification décrivent un problème que réessayer ne corrigera pas —
/// c'est ce qui garantit qu'une mauvaise configuration ne dégénère pas en boucle
/// de connexions sur le NAS.
pub fn is_session_expired(code: i64) -> bool {
    matches!(
        code,
        // 105 : documenté comme « la session n'a pas la permission », mais DSM le
        // renvoie aussi pour une session périmée. On tente donc la reconnexion :
        // si le compte manque vraiment de droits, le second refus produit le
        // message qui parle du groupe « administrators », et la tentative
        // supplémentaire s'arrête là.
        105
        // 106 : session timeout.
        | 106
        // 107 : session interrompue par une connexion en double.
        | 107
        // 119 : session invalide (le `sid` n'est plus connu du NAS).
        | 119
        // 150 : l'IP de la requête ne correspond plus à celle de la connexion,
        // ce qui arrive avec plusieurs interfaces ou après un changement de route.
        | 150
    )
}

/// Traduit un code renvoyé par `SYNO.API.Auth&method=login`.
///
/// Le message doit dire à l'utilisateur quoi faire : c'est le seul retour qu'il
/// aura dans l'interface, et la configuration d'un NAS est l'endroit où l'on se
/// trompe le plus souvent.
pub fn from_auth_code(code: i64) -> ProbeError {
    match code {
        400 => ProbeError::Auth(
            "Unknown account or wrong password. Check the DSM credential entered \
             on the device."
                .to_string(),
        ),
        401 => ProbeError::Auth(
            "This DSM account is disabled. Re-enable it in Control Panel > User & \
             Group, or choose another account."
                .to_string(),
        ),
        402 => ProbeError::Auth(
            "Permission denied: this account is not allowed to sign in to DSM. \
             Allow it the \"DSM\" application in Control Panel > User & Group > \
             Application permissions."
                .to_string(),
        ),
        // 403 et 404 sont le cas fréquent : la vérification en deux étapes est
        // active sur le compte. Un code à usage unique changeant toutes les trente
        // secondes, aucune supervision automatique ne peut le fournir — la seule
        // issue est un compte dédié sans second facteur.
        403 => ProbeError::Auth(TWO_FACTOR_REQUIRED.to_string()),
        404 => ProbeError::Auth(format!(
            "The 2-step verification code was rejected. {TWO_FACTOR_REQUIRED}"
        )),
        406 => ProbeError::Auth(format!(
            "DSM enforces 2-step verification on this account. {TWO_FACTOR_REQUIRED}"
        )),
        407 => ProbeError::Auth(
            "IP address blocked by DSM. Auto block triggered after several failed \
             sign-ins: unblock the DumbMonit server address in Control Panel > \
             Security > Account, then fix the password before re-enabling the \
             device."
                .to_string(),
        ),
        408..=410 => ProbeError::Auth(
            "The password of this DSM account has expired and must be changed before \
             signing in. Change it in DSM, or disable password expiration for the \
             monitoring account."
                .to_string(),
        ),
        // Un appel de connexion peut aussi échouer sur un code commun — API absente
        // sur un DSM trop ancien, système occupé — d'où le repli.
        other => from_common_code(other, "SYNO.API.Auth"),
    }
}

/// Conseil unique sur la vérification en deux étapes, repris par les codes 403,
/// 404 et 406 pour que l'utilisateur lise toujours la même marche à suivre.
const TWO_FACTOR_REQUIRED: &str = "This DSM account requires a 2-step verification \
     code, which automated monitoring cannot enter. Create a dedicated account for \
     DumbMonit in DSM, member of the \"administrators\" group, and leave 2-step \
     verification disabled for that account.";

/// Traduit un code renvoyé par une API de données (`SYNO.Core.System`, etc.).
///
/// `api` n'apparaît que dans le message : il permet de savoir lequel des appels de
/// la collecte a échoué, ce qui est l'information la plus utile quand une seule
/// partie des métriques manque.
pub fn from_api_code(code: i64, api: &str) -> ProbeError {
    from_common_code(code, api)
}

fn from_common_code(code: i64, api: &str) -> ProbeError {
    match code {
        102 => ProbeError::Protocol(format!(
            "The \"{api}\" API does not exist on this NAS: the DSM version is too \
             old, or the matching package is not installed"
        )),
        103 => ProbeError::Protocol(format!("Unknown method on the \"{api}\" API")),
        104 => ProbeError::Protocol(format!(
            "The requested version of the \"{api}\" API is not supported by this NAS"
        )),
        // 105 n'arrive ici qu'après l'unique reconnexion tentée par le client : le
        // doute entre « session périmée » et « droits manquants » est donc levé, et
        // le message peut affirmer la seconde cause.
        105 => ProbeError::Auth(format!(
            "Insufficient permissions for \"{api}\". CPU usage and disk status can \
             only be read by a member of the DSM \"administrators\" group: add the \
             monitoring account to it."
        )),
        106 => ProbeError::Auth(format!(
            "DSM session expired during the \"{api}\" call, and renewing it was \
             not enough"
        )),
        107 => ProbeError::Auth(format!(
            "DSM session interrupted by a duplicate sign-in during the \"{api}\" \
             call. Reserve the monitoring account for DumbMonit."
        )),
        119 => ProbeError::Auth(format!(
            "DSM session invalid during the \"{api}\" call, and renewing it was \
             not enough"
        )),
        150 => ProbeError::Auth(format!(
            "DSM rejected the session on \"{api}\": the request IP address does \
             not match the sign-in address. The DumbMonit server probably goes out \
             through several network interfaces."
        )),
        // Ces codes signalent, dans les termes de Synology, « connexion réseau
        // instable ou système occupé ». C'est la seule famille qui décrive une
        // indisponibilité, donc la seule qui doive alimenter « équipement hors
        // ligne » — les autres sont des erreurs de configuration ou de droits.
        109..=111 | 117 | 118 => ProbeError::Unreachable(format!(
            "The NAS reports an unstable connection or a busy system on \"{api}\" \
             (code {code})"
        )),
        116 => ProbeError::Config("Operation not allowed on a demo installation".to_string()),
        // 160 : droit refusé au niveau de l'application, et non de la session.
        160 => ProbeError::Auth(format!(
            "Access to \"{api}\" is denied to this account at the application level. \
             Check its application permissions in Control Panel > User & Group."
        )),
        // 1055 : « Transmition failed » (la faute de frappe est celle de DSM).
        // Défaut passager de l'API d'utilisation, sans rapport avec un NAS en panne.
        1055 => ProbeError::Protocol(format!(
            "\"{api}\" failed transiently (code 1055); the measurement will be \
             retried on the next poll"
        )),
        other => ProbeError::Protocol(format!("The \"{api}\" API returned Synology error {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_403_de_synology_parle_de_double_authentification_pas_de_droits_http() {
        let error = from_auth_code(403);
        let message = error.to_string();
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(message.contains("2-step"), "{message}");
        assert!(
            message.contains("dedicated account"),
            "le message doit dire quoi faire : {message}"
        );
        assert!(!error.means_down(), "un second facteur manquant n'est pas une panne");
    }

    #[test]
    fn le_404_de_synology_aussi() {
        let message = from_auth_code(404).to_string();
        assert!(message.contains("2-step"), "{message}");
        assert!(message.contains("rejected"), "{message}");
    }

    #[test]
    fn les_refus_didentifiants_sont_des_erreurs_dauthentification() {
        for code in [400, 401, 402, 406, 407, 408, 409, 410] {
            let error = from_auth_code(code);
            assert!(matches!(error, ProbeError::Auth(_)), "code {code} : {error}");
            assert!(!error.means_down(), "le code {code} ne doit pas signaler une panne");
        }
    }

    #[test]
    fn un_compte_desactive_et_un_mot_de_passe_expire_ont_des_messages_distincts() {
        assert!(from_auth_code(401).to_string().contains("disabled"));
        assert!(from_auth_code(409).to_string().contains("expired"));
        assert!(from_auth_code(407).to_string().contains("blocked"));
    }

    #[test]
    fn seuls_les_codes_de_session_perimee_autorisent_une_reconnexion() {
        // 105 en fait partie : DSM l'emploie aussi bien pour une session périmée
        // que pour un manque de droits, et une seule reconnexion tranche.
        for code in [105, 106, 107, 119, 150] {
            assert!(is_session_expired(code), "le code {code} doit permettre un renouvellement");
        }
        for code in [100, 101, 102, 103, 104, 160, 400, 402, 403, 407, 1055] {
            assert!(!is_session_expired(code), "le code {code} ne doit pas être réessayé");
        }
    }

    #[test]
    fn un_manque_de_droits_oriente_vers_le_groupe_administrators() {
        let error = from_api_code(105, "SYNO.Storage.CGI.Storage");
        assert!(matches!(error, ProbeError::Auth(_)));
        let message = error.to_string();
        assert!(message.contains("administrators"), "{message}");
        assert!(message.contains("SYNO.Storage.CGI.Storage"), "l'API fautive doit être nommée");
    }

    #[test]
    fn un_echec_passager_de_lapi_dutilisation_nest_pas_une_panne() {
        let error = from_api_code(1055, "SYNO.Core.System.Utilization");
        assert!(!error.means_down(), "le code 1055 est passager : {error}");
        assert!(error.to_string().contains("transiently"), "{error}");
    }

    #[test]
    fn un_refus_applicatif_est_une_erreur_dauthentification() {
        let error = from_api_code(160, "SYNO.Core.System");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(error.to_string().contains("application permissions"), "{error}");
    }

    #[test]
    fn une_api_absente_est_un_probleme_de_protocole_pas_dauthentification() {
        let error = from_api_code(102, "SYNO.Core.System.SystemHealth");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn seule_la_famille_reseau_signale_un_equipement_hors_ligne() {
        for code in [109, 110, 111, 117, 118] {
            let error = from_api_code(code, "SYNO.Core.System");
            assert!(error.means_down(), "le code {code} décrit une indisponibilité : {error}");
        }
        for code in [100, 101, 102, 105, 114, 116] {
            let error = from_api_code(code, "SYNO.Core.System");
            assert!(!error.means_down(), "le code {code} ne doit pas réveiller quelqu'un");
        }
    }

    #[test]
    fn un_code_inconnu_reste_lisible() {
        let message = from_api_code(9999, "SYNO.Core.System").to_string();
        assert!(message.contains("9999"), "{message}");
        assert!(message.contains("SYNO.Core.System"), "{message}");
    }

    #[test]
    fn aucun_message_derreur_ne_contient_de_secret() {
        // Les messages sont des constantes : aucune valeur venue de la requête, donc
        // ni mot de passe ni `sid`, ne peut s'y retrouver. Ce test fige la propriété.
        for code in [400, 401, 402, 403, 404, 406, 407, 408, 409, 410, 105, 119] {
            let rendu = format!("{}", from_auth_code(code));
            assert!(!rendu.contains("passwd"), "{rendu}");
            assert!(!rendu.contains("_sid"), "{rendu}");
        }
    }
}
