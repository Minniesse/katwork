use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    config::Config,
    docs::{DocLimits, DocPool},
    error::KatError,
    models::Registry,
};

pub struct Core {
    pub events: crate::events::EventHub,
    pub shutdown: tokio_util::sync::CancellationToken,
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub registry: Arc<Registry>,
    pub mailer: Arc<dyn crate::auth::mailer::Mailer>,
    pub docs: Arc<DocPool>,
}

impl Core {
    /// Builds the core and starts the document pool's threads. A thread that cannot be spawned
    /// is this function's error, so the server does not start without its pool.
    pub fn start(pool: PgPool, config: Arc<Config>) -> Result<Self, KatError> {
        let registry = Arc::new(Registry::from_config(&config));
        let mailer = crate::auth::mailer::from_config(&config);
        let docs = Arc::new(DocPool::new(config.doc_workers, doc_limits(&config))?);
        Ok(Self {
            events: crate::events::EventHub::new(),
            shutdown: tokio_util::sync::CancellationToken::new(),
            pool,
            config,
            registry,
            mailer,
            docs,
        })
    }

    /// `start` for tests and tools, which have no startup error to report.
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        Self::start(pool, config).expect("start the document pool")
    }

    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = Arc::new(registry);
        self
    }

    /// Replaces the document pool (tests use small call timeouts).
    pub fn with_docs(mut self, docs: Arc<DocPool>) -> Self {
        self.docs = docs;
        self
    }
}

fn doc_limits(config: &Config) -> DocLimits {
    let limits = DocLimits {
        max_xml_elements: config.doc_max_xml_elements,
        ..DocLimits::default()
    };
    // The smoke stack wedges the pool on purpose and needs the watchdog to fire in seconds.
    #[cfg(feature = "smoke-fixtures")]
    let limits = match std::env::var("KATWORK_SMOKE_DOC_CALL_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&millis| millis > 0)
    {
        Some(millis) => DocLimits {
            call_timeout: std::time::Duration::from_millis(millis),
            wedge_exit_after: std::time::Duration::from_millis(millis),
            ..limits
        },
        None => limits,
    };
    limits
}
