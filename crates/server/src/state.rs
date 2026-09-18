use std::sync::Arc;

use sqlx::SqlitePool;

use crate::collectors::Registry;
use crate::collectors::relay::RelayHub;
use crate::config::Config;
use crate::crypto::Cipher;
use crate::tsdb::{SampleSink, Victoria};

/// État partagé par tous les gestionnaires HTTP et par le planificateur.
#[derive(Clone)]
pub struct AppState(Arc<Inner>, Arc<RelayHub>);

pub struct Inner {
    pub config: Config,
    pub pool: SqlitePool,
    pub cipher: Cipher,
    pub victoria: Victoria,
    pub sink: SampleSink,
    pub collectors: Registry,
}

impl AppState {
    pub fn new(inner: Inner) -> Self {
        Self(Arc::new(inner), Arc::new(RelayHub::new()))
    }

    /// Sondes déléguées aux agents relais. Construite avec l'état plutôt que
    /// fournie par l'appelant : elle n'a aucune dépendance, et chaque instance
    /// (serveur ou test) doit avoir la sienne.
    pub fn relay(&self) -> &RelayHub {
        &self.1
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
