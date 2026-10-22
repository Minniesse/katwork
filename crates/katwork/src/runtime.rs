use std::sync::Arc;

use sqlx::PgPool;

use crate::config::Config;

pub struct Core {
    pub pool: PgPool,
    pub config: Arc<Config>,
}

impl Core {
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        Self { pool, config }
    }
}
