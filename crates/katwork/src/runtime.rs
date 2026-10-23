use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;

pub struct Core {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub mailer: Arc<dyn crate::auth::mailer::Mailer>,
}

impl Core {
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        let mailer = crate::auth::mailer::from_config(&config);
        Self {
            pool,
            config,
            mailer,
        }
    }
}
