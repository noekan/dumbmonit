//! Canal de commandes du serveur vers l'agent.
//!
//! Le sens reste celui de la poussée : c'est l'agent qui vient chercher ses
//! commandes après chaque lot envoyé, et qui rend compte de leur exécution. Le
//! serveur n'ouvre jamais de connexion vers la machine surveillée, et une machine
//! dont l'agent est arrêté ne reçoit simplement rien.

use serde::{Deserialize, Serialize};

use crate::{Sample, Target};

/// Route de récupération (`GET`) et de compte rendu (`POST …/{id}`) des commandes.
pub const COMMANDS_PATH: &str = "/api/agent/commands";

/// Âge au-delà duquel une commande n'est plus exécutée.
///
/// Une commande attend l'agent ; si celui-ci a été coupé dix minutes, la panne qui
/// l'a motivée a probablement changé de nature, et redémarrer un conteneur sur
/// une machine qu'on est peut-être en train de réparer n'aiderait personne.
pub const COMMAND_MAX_AGE_SECS: u64 = 600;

/// Redémarre un conteneur. Arguments : `{"name": "…"}`.
pub const CMD_CONTAINER_RESTART: &str = "container.restart";

/// Remplace un conteneur par la même image, tirée à nouveau. Arguments :
/// `{"name": "…", "prune": bool}`.
pub const CMD_CONTAINER_UPDATE: &str = "container.update";

/// Interroge un équipement à la place du serveur (agent relais). Arguments :
/// un [`ProbeJob`] sérialisé.
pub const CMD_PROBE: &str = "probe";

/// Route des sondes déléguées : `GET` (attente longue) pour les recevoir,
/// `POST …/{id}` pour en rendre compte. Distincte de [`COMMANDS_PATH`] : les
/// sondes voyagent avec l'identifiant de l'équipement en clair et ne sont jamais
/// écrites en base, là où les commandes sont journalisées.
pub const RELAY_PATH: &str = "/api/agent/relay";

/// Durée pendant laquelle le serveur retient une demande de sondes sans rien
/// avoir à donner, avant de répondre « rien ». L'agent renvoie aussitôt une
/// nouvelle demande : le délai entre l'échéance d'une cible et son interrogation
/// se compte ainsi en millisecondes, sans que l'agent ne martèle le serveur.
pub const RELAY_POLL_HOLD_SECS: u64 = 25;

/// Une sonde à exécuter par un agent relais, telle qu'elle voyage dans les
/// arguments d'une commande [`CMD_PROBE`].
///
/// La cible est transmise entière, secret compris : c'est l'agent qui va se
/// présenter à l'équipement. Le canal est celui des mesures (jeton porteur, et
/// TLS dès que le serveur est publié derrière un mandataire), et rien de tout
/// cela n'est jamais écrit sur disque, ni côté serveur ni côté agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeJob {
    pub target: Target,
    /// Délai maximal de la sonde, en secondes, le même que sur le serveur.
    pub timeout_secs: u64,
    /// Identifier l'équipement (profil SNMP) plutôt que le mesurer.
    #[serde(default)]
    pub discover: bool,
}

impl ProbeJob {
    /// Construit la commande à remettre à l'agent.
    pub fn into_command(self, id: i64, created_at_ms: i64) -> AgentCommand {
        AgentCommand {
            id,
            kind: CMD_PROBE.to_string(),
            args: serde_json::to_value(self).unwrap_or(serde_json::Value::Null),
            created_at_ms,
        }
    }

    /// Relit la sonde dans une commande reçue.
    pub fn from_command(command: &AgentCommand) -> Result<Self, String> {
        if command.kind != CMD_PROBE {
            return Err(format!("not a probe command: {}", command.kind));
        }
        serde_json::from_value(command.args.clone())
            .map_err(|error| format!("unreadable probe job: {error}"))
    }
}

/// Compte rendu d'une sonde déléguée.
///
/// Les échantillons voyagent avec le verdict : un seul aller-retour, et le
/// serveur les range sous l'équipement sondé exactement comme s'il l'avait
/// interrogé lui-même. `error` reprend le texte de `ProbeError` : son préfixe
/// dit au serveur si l'équipement est injoignable ou mal configuré.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    pub duration_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub samples: Vec<Sample>,
    /// Profil reconnu, pour une demande d'identification.
    #[serde(default)]
    pub profile_id: Option<String>,
}

/// Une commande en attente, telle que le serveur la remet à l'agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCommand {
    pub id: i64,
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Value,
    /// Instant de la demande, en millisecondes depuis l'époque Unix : c'est ce
    /// qui permet à l'agent d'écarter une commande trop ancienne.
    pub created_at_ms: i64,
}

impl AgentCommand {
    /// Vrai si la commande est trop ancienne pour être exécutée sans risque.
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) > (COMMAND_MAX_AGE_SECS as i64) * 1000
    }

    /// Argument texte, s'il est présent et non vide.
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(|value| value.as_str()).map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn arg_bool(&self, key: &str) -> Option<bool> {
        self.args.get(key).and_then(|value| value.as_bool())
    }
}

/// État d'une commande. Les quatre derniers sont finaux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandStatus {
    Queued,
    Running,
    Done,
    Failed,
    /// Retirée de la file par un utilisateur avant que l'agent ne la prenne.
    Cancelled,
    /// Restée en attente plus de [`COMMAND_MAX_AGE_SECS`] : l'agent n'est jamais
    /// venu la chercher (arrêté, trop ancien, ou actions désactivées).
    Expired,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }

    /// Vrai si plus rien ne changera.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled | Self::Expired)
    }
}

/// Compte rendu de l'agent sur une commande.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReport {
    pub status: CommandStatus,
    /// Extrait du journal d'exécution, lisible tel quel dans l'interface.
    #[serde(default)]
    pub result: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_expires_after_ten_minutes() {
        let command = AgentCommand {
            id: 1,
            kind: CMD_CONTAINER_RESTART.into(),
            args: serde_json::json!({ "name": "web" }),
            created_at_ms: 1_000_000,
        };
        assert!(!command.is_expired(1_000_000 + 599_000));
        assert!(command.is_expired(1_000_000 + 601_000));
        // Une horloge serveur en avance ne rend pas la commande périmée.
        assert!(!command.is_expired(0));
    }

    #[test]
    fn arguments_are_read_leniently() {
        let command = AgentCommand {
            id: 1,
            kind: CMD_CONTAINER_UPDATE.into(),
            args: serde_json::json!({ "name": "  web ", "prune": true, "empty": "" }),
            created_at_ms: 0,
        };
        assert_eq!(command.arg_str("name"), Some("web"));
        assert_eq!(command.arg_str("empty"), None);
        assert_eq!(command.arg_str("missing"), None);
        assert_eq!(command.arg_bool("prune"), Some(true));
    }

    #[test]
    fn statuses_round_trip_through_their_text_form() {
        for status in [
            CommandStatus::Queued,
            CommandStatus::Running,
            CommandStatus::Done,
            CommandStatus::Failed,
            CommandStatus::Cancelled,
            CommandStatus::Expired,
        ] {
            assert_eq!(CommandStatus::parse(status.as_str()), Some(status));
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
        assert!(CommandStatus::Done.is_final());
        assert!(CommandStatus::Expired.is_final());
        assert!(!CommandStatus::Running.is_final());
        assert_eq!(CommandStatus::parse("bogus"), None);
    }

    #[test]
    fn a_probe_job_survives_the_command_envelope() {
        let job = ProbeJob {
            target: Target {
                id: 12,
                name: "site-web".into(),
                address: "https://example.org".into(),
                kind: "http".into(),
                profile_id: None,
                parent_id: None,
                interval: std::time::Duration::from_secs(60),
                enabled: true,
                tags: Default::default(),
                credential: crate::Credential::ApiToken { token: "s3cr3t".into() },
            },
            timeout_secs: 10,
            discover: false,
        };
        let command = job.into_command(7, 1_000);
        assert_eq!(command.kind, CMD_PROBE);
        let back = ProbeJob::from_command(&command).expect("relecture");
        assert_eq!(back.target.id, 12);
        assert_eq!(back.timeout_secs, 10);
        assert_eq!(back.target.credential, crate::Credential::ApiToken { token: "s3cr3t".into() });

        let other = AgentCommand {
            id: 1,
            kind: CMD_CONTAINER_RESTART.into(),
            args: serde_json::json!({}),
            created_at_ms: 0,
        };
        assert!(ProbeJob::from_command(&other).is_err());
    }

    #[test]
    fn a_report_without_result_is_valid() {
        let report: CommandReport = serde_json::from_str(r#"{"status":"running"}"#).unwrap();
        assert_eq!(report, CommandReport { status: CommandStatus::Running, result: String::new() });
    }
}
