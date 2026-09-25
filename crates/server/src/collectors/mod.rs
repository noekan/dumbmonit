//! Registre des collecteurs.
//!
//! Les collecteurs réseau (SNMP, Proxmox, PBS, PDM, Synology, sondes de disponibilité)
//! vivent dans la caisse `dumbmonit-collectors`, partagée avec l'agent relais ;
//! ils sont réexportés ici sous leurs anciens chemins (`collectors::proxmox`, …)
//! pour que le reste du serveur n'ait pas à savoir où ils habitent. Le collecteur
//! « agent » (fraîcheur des mesures poussées, commandes, jetons) reste ici : il
//! lit la base.
//!
//! Ajouter une intégration se résume à implémenter [`Collector`] et à l'enregistrer
//! dans le [`Registry`] : le planificateur et l'API n'ont pas connaissance des
//! types concrets.

pub mod agent;
pub mod opnsense_history;
pub mod pbs_history;
pub mod pdm_history;
pub mod pmg_history;
pub mod push;
pub mod relay;
pub mod synology_history;
pub mod truenas_history;

pub use dumbmonit_collectors::{
    DnsCollector, DummyCollector, HttpCollector, OpnsenseCollector, PbsCollector, PdmCollector,
    PingCollector, PmgCollector, ProxmoxCollector, Registry, SnmpCollector, SynologyCollector,
    TcpCollector, TlsCollector, TruenasCollector, dummy, http, opnsense, pbs, pdm, pmg, proxmox,
    snmp, synology, truenas, uptime,
};
pub use dumbmonit_collectors::{
    MqttCollector, MysqlCollector, PostgresCollector, SmtpCollector, WebsocketCollector,
};
pub use dumbmonit_collectors::{RedfishCollector, redfish};
#[allow(unused_imports)]
pub use dumbmonit_proto::Collector;

pub use agent::AgentCollector;
pub use push::PushCollector;
