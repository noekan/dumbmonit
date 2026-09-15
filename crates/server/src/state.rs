use std::sync::Arc;

use sqlx::SqlitePool;

use crate::collectors::Registry;
use crate::config::Config;
use crate::crypto::Cipher;
use crate::tsdb::{SampleSink, Victoria};

/// État partagé par tous les gestionnaires HTTP et par le planificateur.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

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
        Self(Arc::new(inner))
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
