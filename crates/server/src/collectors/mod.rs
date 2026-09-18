//! Registre des collecteurs.
//!
//! Les collecteurs réseau (SNMP, Proxmox, PBS, Synology, sondes de disponibilité)
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
pub mod pbs_history;
pub mod relay;
pub mod synology_history;

pub use dumbmonit_collectors::{
    DnsCollector, DummyCollector, HttpCollector, PbsCollector, PingCollector, ProxmoxCollector,
    Registry, SnmpCollector, SynologyCollector, TcpCollector, TlsCollector, dummy, http, pbs,
    proxmox, snmp, synology, uptime,
};
#[allow(unused_imports)]
pub use dumbmonit_proto::Collector;

pub use agent::AgentCollector;
