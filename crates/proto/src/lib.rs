//! Types partagés entre le serveur, les collecteurs et l'agent.
//!
//! Ce crate ne dépend d'aucun runtime ni d'aucune base de données : il définit
//! uniquement le contrat de données. C'est ce qui garantit qu'un collecteur SNMP,
//! l'agent distant et le serveur parlent tous exactement le même langage.

mod collector;
mod command;
mod credential;
pub mod env;
mod push;
mod sample;
mod target;

pub use collector::{Collector, ProbeError};
pub use command::{
    AgentCommand, CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE, CMD_PROBE, COMMAND_MAX_AGE_SECS,
    COMMANDS_PATH, CommandReport, CommandStatus, ProbeJob, ProbeOutcome, RELAY_PATH,
    RELAY_POLL_HOLD_SECS,
};
pub use credential::{
    Credential, SnmpV3Auth, SnmpV3AuthProtocol, SnmpV3Privacy, SnmpV3PrivacyProtocol,
};
pub use push::{
    AgentIdentity, INGEST_PATH, MAX_BATCH_SAMPLES, PUSH_PROTOCOL_VERSION, PushAck, PushBatch,
    TOKEN_PREFIX,
};
pub use sample::{MetricKind, Sample};
pub use target::{Target, TargetId};
