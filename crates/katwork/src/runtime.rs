use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, models::Registry};

pub struct Core {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub registry: Arc<Registry>,
}

impl Core {
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        let registry = Arc::new(Registry::from_config(&config));
        Self {
            pool,
            config,
            registry,
        }
    }

    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Arc::new(registry);
        self
    }
}
