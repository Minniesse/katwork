use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, models::Registry};

pub struct Core {
    pub events: crate::events::EventHub,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub registry: Arc<Registry>,
    pub mailer: Arc<dyn crate::auth::mailer::Mailer>,
}

impl Core {
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        let registry = Arc::new(Registry::from_config(&config));
        let mailer = crate::auth::mailer::from_config(&config);
        Self {
            events: crate::events::EventHub::new(),
            shutdown: tokio_util::sync::CancellationToken::new(),
            pool,
            config,
            registry,
            mailer,
        }
    }

    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Arc::new(registry);
        self
    }
}
