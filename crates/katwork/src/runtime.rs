use std::{sync::Arc, time::Duration};

use sqlx::PgPool;

use crate::{
    artifacts::{
        BlobStore, LocalBlobStore, blob,
        uploads::{UPLOADS_AT_ONCE, UPLOADS_PER_PERSON, UploadLimiter},
    },
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
    pub blobs: Arc<dyn BlobStore>,
    pub uploads: Arc<UploadLimiter>,
    /// The upload commits running in tasks of their own, which a client that leaves cannot
    /// drop; `finish_commits` waits for them at shutdown.
    pub commits: tokio_util::task::TaskTracker,
}

impl Core {
    /// Builds the core: checks that the artifact directory is writable, then starts the
    /// document pool's threads. Either failure is this function's error, so the server does not
    /// start without its files or its pool.
    pub fn start(pool: PgPool, config: Arc<Config>) -> Result<Self, KatError> {
        let blobs = Arc::new(LocalBlobStore::new(config.artifact_dir.clone())?);
        Self::assemble(pool, config, blobs)
    }

    /// `start` for tests and tools, which have no startup error to report. It touches no
    /// directory: its blob store refuses every call until `with_blob_store` replaces it.
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        Self::assemble(pool, config, Arc::new(blob::Unconfigured)).expect("start the document pool")
    }

    fn assemble(
        pool: PgPool,
        config: Arc<Config>,
        blobs: Arc<dyn BlobStore>,
    ) -> Result<Self, KatError> {
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
            blobs,
            uploads: Arc::new(UploadLimiter::new(UPLOADS_AT_ONCE, UPLOADS_PER_PERSON)),
            commits: tokio_util::task::TaskTracker::new(),
        })
    }

    /// Waits, at most `grace`, for the upload commits still running once the server has
    /// stopped taking requests; false when one outlived it. A commit left then ends with the
    /// process, as in a crash: its blob may stand without its rows.
    pub async fn finish_commits(&self, grace: Duration) -> bool {
        self.commits.close();
        tokio::time::timeout(grace, self.commits.wait())
            .await
            .is_ok()
    }

    /// Replaces the blob store (tests use it).
    pub fn with_blob_store(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = blobs;
        self
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
        max_read_bytes: config.doc_max_read_bytes,
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
