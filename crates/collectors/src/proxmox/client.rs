//! Accès HTTP à l'API Proxmox VE.
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration
//! ne manipule que des structures déjà désérialisées, ce qui le rend testable
//! sans serveur en face.

use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::auth::{AuthMode, Ticket};
use super::model::{Envelope, TicketResponse};

/// Longueur maximale du corps d'erreur repris dans un message.
///
/// Proxmox renvoie parfois une page HTML complète : on garde de quoi diagnostiquer
/// sans inonder les journaux.
const MAX_ERROR_BODY: usize = 200;

pub struct PveClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthMode,
    timeout: Duration,
}

/// Échec d'un appel, avec le statut HTTP quand le serveur a répondu.
///
/// Le statut reste interne au client : le reste de l'intégration raisonne en
/// `ProbeError`, sauf pour décider qu'un 403 ou un 501 est un cas prévu.
struct Failure {
    status: Option<StatusCode>,
    /// Le serveur a répondu « cette fonctionnalité n'est pas là » plutôt que
    /// « je suis en panne » — voir [`absence_marker`].
    absent: bool,
    error: ProbeError,
}

impl From<ProbeError> for Failure {
    fn from(error: ProbeError) -> Self {
        Self { status: None, absent: false, error }
    }
}

impl PveClient {
    pub fn new(http: reqwest::Client, base_url: String, auth: AuthMode, timeout: Duration) -> Self {
        Self { http, base_url, auth, timeout }
    }

    /// Interroge un chemin de l'API et déballe l'enveloppe `{"data": …}`.
    ///
    /// Un 401 sur une session par ticket déclenche exactement un renouvellement :
    /// le ticket a pu expirer entre deux interrogations, mais un jeton refusé deux
    /// fois de suite est une erreur de configuration, pas un incident passager.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ProbeError> {
        self.fetch(path, query).await.map_err(|failure| failure.error)
    }

    /// Comme [`get`](Self::get), mais certains statuts HTTP sont un cas prévu
    /// et donnent `Ok(None)` plutôt qu'une erreur.
    ///
    /// Deux usages : un endpoint facultatif que le rôle `PVEAuditor` ne couvre
    /// pas (403 sur `/nodes/{node}/apt/update`), et une fonctionnalité absente de
    /// l'installation (404 ou 501 sur la réplication d'une machine isolée). Dans
    /// les deux cas, il n'y a rien à compter comme erreur ni à journaliser au-delà
    /// du niveau `debug`.
    ///
    /// Les 500 qui décrivent une absence (voir [`absence_marker`]) donnent
    /// toujours `Ok(None)`, sans avoir à les énumérer : Proxmox ne réserve pas
    /// de statut à « pas installé ».
    pub async fn get_unless<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        expected: &[StatusCode],
    ) -> Result<Option<T>, ProbeError> {
        match self.fetch(path, query).await {
            Ok(data) => Ok(Some(data)),
            Err(failure)
                if failure.absent
                    || failure.status.is_some_and(|status| expected.contains(&status)) =>
            {
                Ok(None)
            }
            Err(failure) => Err(failure.error),
        }
    }

    /// Comme [`get`](Self::get), pour un endpoint dont l'absence est un cas
    /// normal : Ceph non installé, agent QEMU arrêté, invité disparu entre deux
    /// appels. `Ok(None)` veut dire « pas là », et seule une vraie panne reste
    /// une erreur.
    pub async fn get_if_present<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<T>, ProbeError> {
        self.get_unless(path, query, &[]).await
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, Failure> {
        let response = self.send(path, query, false).await?;

        let response = if response.status() == StatusCode::UNAUTHORIZED
            && matches!(self.auth, AuthMode::Ticket { .. })
        {
            self.send(path, query, true).await?
        } else {
            response
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Failure {
                status: Some(status),
                absent: absence_marker(status, &body).is_some(),
                error: status_error(status, &body, path),
            });
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, path, self.timeout))?;
        let envelope: Envelope<T> = serde_json::from_slice(&body).map_err(|error| {
            ProbeError::Protocol(format!("Unexpected response from {path}: {error}"))
        })?;
        Ok(envelope.data)
    }

    async fn send(
        &self,
        path: &str,
        query: &[(&str, String)],
        renew_ticket: bool,
    ) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}/api2/json{path}", self.base_url);
        let mut request = self.http.get(&url).timeout(self.timeout);
        if !query.is_empty() {
            request = request.query(query);
        }

        request = match &self.auth {
            AuthMode::Token(header) => request.header(reqwest::header::AUTHORIZATION, header),
            AuthMode::Ticket { .. } => {
                let ticket = self.ticket(renew_ticket).await?;
                request.header(reqwest::header::COOKIE, format!("PVEAuthCookie={ticket}"))
            }
        };

        request.send().await.map_err(|error| map_transport(&error, path, self.timeout))
    }

    /// Renvoie un ticket utilisable, en le renouvelant si nécessaire.
    ///
    /// Le verrou est tenu pendant l'appel à `/access/ticket` pour qu'une rafale de
    /// requêtes parallèles n'ouvre pas autant de sessions sur le serveur.
    async fn ticket(&self, force_renew: bool) -> Result<String, ProbeError> {
        let AuthMode::Ticket { username, password, cached } = &self.auth else {
            return Err(ProbeError::Config("Unexpected authentication mode".to_string()));
        };

        let mut slot = cached.lock().await;
        let now_s = chrono::Utc::now().timestamp();

        if !force_renew
            && let Some(ticket) = slot.as_ref()
            && ticket.is_usable_at(now_s)
        {
            return Ok(ticket.ticket.clone());
        }

        let fresh = self.request_ticket(username, password, now_s).await?;
        let value = fresh.ticket.clone();
        *slot = Some(fresh);
        Ok(value)
    }

    async fn request_ticket(
        &self,
        username: &str,
        password: &str,
        now_s: i64,
    ) -> Result<Ticket, ProbeError> {
        const PATH: &str = "/access/ticket";
        let url = format!("{}/api2/json{PATH}", self.base_url);

        let response = self
            .http
            .post(&url)
            .timeout(self.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_urlencoded(&[("username", username), ("password", password)]))
            .send()
            .await
            .map_err(|error| map_transport(&error, PATH, self.timeout))?;

        let status = response.status();
        if !status.is_success() {
            // Le corps d'une réponse d'authentification peut refléter les
            // identifiants soumis : on ne le reprend jamais dans le message.
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProbeError::Auth(
                    "Proxmox rejected the credentials (username, password or realm)".to_string(),
                ),
                _ => status_error(status, "", PATH),
            });
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, PATH, self.timeout))?;
        let envelope: Envelope<TicketResponse> = serde_json::from_slice(&body).map_err(|_| {
            ProbeError::Protocol("Unexpected response from /access/ticket".to_string())
        })?;

        Ok(Ticket { ticket: envelope.data.ticket, acquired_at: now_s })
    }
}

/// Encode un corps `application/x-www-form-urlencoded`.
///
/// Écrit à la main faute d'encodeur dans les fonctionnalités retenues de
/// `reqwest`. Un mot de passe contient volontiers `+`, `&` ou `%` : le laisser
/// passer tel quel produirait un refus incompréhensible à l'authentification.
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        encode_into(&mut body, key);
        body.push('=');
        encode_into(&mut body, value);
    }
    body
}

fn encode_into(out: &mut String, value: &str) {
    for byte in value.as_bytes() {
        match byte {
            // Caractères « non réservés » de la RFC 3986, seuls à passer tels quels.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

/// Traduit une erreur de transport en `ProbeError`.
///
/// La distinction est structurante : seuls `Timeout` et `Unreachable` alimentent
/// l'alerte « équipement hors ligne ». Un certificat refusé est classé en
/// configuration, sans quoi tout homelab en certificat auto-signé afficherait son
/// hyperviseur comme éteint.
fn map_transport(error: &reqwest::Error, path: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if is_certificate_error(error) {
        return ProbeError::Config(
            "TLS certificate rejected: a Proxmox installation uses a self-signed \
             certificate by default. Add the tag \"insecure_tls = true\" on the device \
             to knowingly accept it, or install a trusted certificate on the \
             hypervisor."
                .to_string(),
        );
    }
    if error.is_decode() {
        return ProbeError::Protocol(format!("Unreadable response from {path}"));
    }
    ProbeError::Unreachable(format!("{path}: {}", cause_chain(error)))
}

/// Vrai si l'échec vient de la validation du certificat.
///
/// `reqwest` range ces erreurs parmi les erreurs de connexion et n'expose pas de
/// prédicat dédié : on inspecte donc la chaîne de causes, qui contient le libellé
/// de `rustls`.
fn is_certificate_error(error: &reqwest::Error) -> bool {
    let chain = cause_chain(error).to_lowercase();
    ["certificate", "certificat", "unknownissuer", "notvalidfor", "certexpired", "tls"]
        .iter()
        .any(|motif| chain.contains(motif))
}

fn cause_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Tournures par lesquelles Proxmox dit « cette fonctionnalité n'est pas là ».
///
/// L'API ne réserve aucun statut à l'absence : elle répond 500 avec le message
/// de l'outil qu'elle n'a pas pu lancer. Un cluster sans Ceph rend
/// `binary not installed: /usr/bin/ceph-mon`, une VM dont l'agent est arrêté
/// `QEMU guest agent is not running`, un nœud sans ZFS
/// `binary not installed: /sbin/zpool`. Les reconnaître est la seule façon de
/// distinguer « pas installé » de « en panne », qui partagent le même code.
///
/// Comparées en minuscules, sur le corps entier : le message utile est parfois
/// noyé dans une enveloppe JSON ou une page HTML.
const ABSENCE_MARKERS: [&str; 9] = [
    // Ceph, ZFS, LVM… : l'exécutable n'est pas sur le disque.
    "binary not installed",
    "is not installed",
    "command not found",
    // Ceph installé mais jamais initialisé.
    "rados_connect failed",
    "not initialized",
    // Agent QEMU : absent du système invité, arrêté, ou muet.
    "guest agent is not running",
    "agent is not enabled",
    "qga command",
    "qmp command",
];

/// Le motif d'absence trouvé dans le corps d'une réponse en échec.
///
/// Réservé aux 5xx : un 404 ou un 403 se lit déjà à son statut, et le corps
/// d'une réponse réussie ne veut rien dire ici.
pub(super) fn absence_marker(status: StatusCode, body: &str) -> Option<&'static str> {
    if !status.is_server_error() {
        return None;
    }
    let body = body.to_ascii_lowercase();
    ABSENCE_MARKERS.into_iter().find(|marker| body.contains(marker))
}

/// Traduit un code de statut HTTP en `ProbeError`.
pub(super) fn status_error(status: StatusCode, body: &str, path: &str) -> ProbeError {
    match status {
        StatusCode::UNAUTHORIZED => ProbeError::Auth(
            "Proxmox refused authentication: invalid or revoked token, or wrong \
             credentials"
                .to_string(),
        ),
        StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "Insufficient permissions on {path}: grant at least the PVEAuditor role \
             on \"/\" to the user or token"
        )),
        // Une 500 qui dit « pas installé » ou « pas démarré » décrit une
        // fonctionnalité absente, pas une panne : elle ne doit jamais réveiller
        // personne, même si l'appelant la traite comme une erreur ordinaire.
        s if absence_marker(s, body).is_some() => {
            ProbeError::Protocol(format!("{path}: {} {}", s.as_u16(), summarize(body)))
        }
        // La 5xx recouvre les codes 59x du proxy Proxmox, qui signalent un nœud du
        // cluster qui ne répond pas — donc bien une indisponibilité.
        s if s.is_server_error() => {
            ProbeError::Unreachable(format!("{path}: {} {}", s.as_u16(), summarize(body)))
        }
        s => ProbeError::Protocol(format!("{path}: {} {}", s.as_u16(), summarize(body))),
    }
}

fn summarize(body: &str) -> String {
    let body: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match body.char_indices().nth(MAX_ERROR_BODY) {
        Some((index, _)) => format!("{}…", &body[..index]),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_401_est_une_erreur_dauthentification_et_pas_une_panne() {
        let error = status_error(StatusCode::UNAUTHORIZED, "", "/version");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(!error.means_down(), "un jeton invalide ne doit réveiller personne");
    }

    #[test]
    fn un_403_oriente_vers_les_droits_manquants() {
        let error = status_error(StatusCode::FORBIDDEN, "", "/nodes/pve1/status");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(error.to_string().contains("PVEAuditor"));
    }

    #[test]
    fn une_5xx_signale_un_noeud_indisponible() {
        // 596 : code propre au proxy Proxmox quand le nœud visé ne répond pas.
        let error = status_error(StatusCode::from_u16(596).unwrap(), "", "/nodes/pve3/status");
        assert!(error.means_down());
    }

    #[test]
    fn une_4xx_ordinaire_est_une_erreur_de_protocole() {
        let error = status_error(StatusCode::NOT_FOUND, "no such node", "/nodes/absent/status");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn le_corps_derreur_est_tronque() {
        let body = "x".repeat(1000);
        let error = status_error(StatusCode::BAD_REQUEST, &body, "/version");
        assert!(error.to_string().len() < 300, "{error}");
    }

    #[test]
    fn le_corps_derreur_est_mis_sur_une_seule_ligne() {
        let error = status_error(StatusCode::BAD_REQUEST, "erreur\n  détaillée", "/version");
        assert!(error.to_string().ends_with("400 erreur détaillée"), "{error}");
    }

    #[test]
    fn le_corps_de_formulaire_echappe_les_caracteres_speciaux() {
        let body = form_urlencoded(&[("username", "root@pam"), ("password", "a+b&c=d %ç")]);
        assert_eq!(body, "username=root%40pam&password=a%2Bb%26c%3Dd%20%25%C3%A7");
    }

    #[test]
    fn le_corps_de_formulaire_preserve_les_caracteres_non_reserves() {
        assert_eq!(form_urlencoded(&[("a", "Az0-_.~")]), "a=Az0-_.~");
    }

    /// Corps relevés sur un cluster Proxmox VE 9.2 sans Ceph : les six endpoints
    /// Ceph répondent 500 avec ce message, dans un ordre de clés variable.
    const CEPH_ABSENT: [&str; 2] = [
        r#"{"data":null,"message":"binary not installed: /usr/bin/ceph-mon\n"}"#,
        r#"{"message":"binary not installed: /usr/bin/ceph-mon\n","data":null}"#,
    ];

    /// Corps relevé sur une VM dont l'agent QEMU est déclaré mais arrêté.
    const AGENT_ABSENT: [&str; 2] = [
        r#"{"message":"QEMU guest agent is not running\n","data":null}"#,
        r#"{"data":null,"message":"QEMU guest agent is not running\n"}"#,
    ];

    /// Corps relevé sur les trois nœuds pour `apt/update`, faute de `Sys.Modify`.
    const APT_REFUSE: &str =
        r#"{"message":"Permission check failed (/nodes/node3, Sys.Modify)\n","data":null}"#;

    #[test]
    fn un_cluster_sans_ceph_est_une_absence_et_pas_une_panne() {
        for body in CEPH_ABSENT {
            assert_eq!(
                absence_marker(StatusCode::INTERNAL_SERVER_ERROR, body),
                Some("binary not installed"),
                "{body}"
            );
            let error =
                status_error(StatusCode::INTERNAL_SERVER_ERROR, body, "/cluster/ceph/status");
            assert!(!error.means_down(), "Ceph absent ne doit réveiller personne : {error}");
        }
    }

    #[test]
    fn un_agent_qemu_arrete_est_une_absence_et_pas_une_panne() {
        for body in AGENT_ABSENT {
            assert_eq!(
                absence_marker(StatusCode::INTERNAL_SERVER_ERROR, body),
                Some("guest agent is not running"),
                "{body}"
            );
            let error = status_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                body,
                "/nodes/node2/qemu/107/agent/get-osinfo",
            );
            assert!(!error.means_down(), "{error}");
        }
    }

    #[test]
    fn un_noeud_sans_zfs_est_une_absence() {
        let body = r#"{"data":null,"message":"binary not installed: /sbin/zpool\n"}"#;
        assert!(absence_marker(StatusCode::INTERNAL_SERVER_ERROR, body).is_some());
    }

    #[test]
    fn une_vraie_panne_reste_une_panne() {
        // Le proxy Proxmox devant un nœud qui ne répond plus, et une erreur
        // interne sans explication : les deux doivent rester « hors ligne ».
        for (status, body) in [(596, "Connection refused"), (500, "internal error")] {
            let error =
                status_error(StatusCode::from_u16(status).unwrap(), body, "/nodes/node3/status");
            assert!(error.means_down(), "{status} {body}");
        }
    }

    #[test]
    fn labsence_ne_se_lit_que_sur_une_5xx() {
        // Le même texte sur un 404 reste un 404 : le statut suffit déjà à le dire.
        assert_eq!(absence_marker(StatusCode::NOT_FOUND, "binary not installed"), None);
        assert_eq!(absence_marker(StatusCode::FORBIDDEN, "binary not installed"), None);
    }

    #[test]
    fn un_403_sur_apt_update_reste_un_droit_manquant() {
        // `Sys.Modify` non accordé : ni absence, ni panne — un statut connu, que
        // `get_unless` transforme en `Ok(None)`.
        assert_eq!(absence_marker(StatusCode::FORBIDDEN, APT_REFUSE), None);
        let error = status_error(StatusCode::FORBIDDEN, APT_REFUSE, "/nodes/node3/apt/update");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn le_message_derreur_dauthentification_ne_contient_aucun_secret() {
        let error = status_error(StatusCode::UNAUTHORIZED, "token=SECRET-JETON", "/version");
        assert!(!error.to_string().contains("SECRET-JETON"));
    }
}
