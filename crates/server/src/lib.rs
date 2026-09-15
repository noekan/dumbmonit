//! Serveur EzyMonit.
//!
//! Le code vit dans une bibliothèque plutôt que directement dans le binaire, afin
//! que les tests d'intégration puissent monter l'application complète — routeur,
//! base et collecteurs — et l'exercer comme le ferait un client.

pub mod alerting;
pub mod api;
pub mod auth;
pub mod collectors;
pub mod config;
pub mod crypto;
pub mod db;
pub mod notify;
pub mod scheduler;
pub mod state;
pub mod tsdb;
