#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;

async fn single_connection(f: &mut Fixture) -> (PgPool, i32) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*f.core.pool.connect_options()).clone())
        .await
        .unwrap();
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    f.core = Arc::new(Core::new(pool.clone(), f.core.config.clone()));
    f.app = build_router(AppState {
        core: f.core.clone(),
    });
    (pool, pid)
}

async fn wait_for_backend_lock(pool: &PgPool, pid: i32) {
    timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')")
                .bind(pid).fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("database operation did not block");
}

async fn assert_replaced(pool: &PgPool, old_pid: i32) {
    let pid: i32 = timeout(
        Duration::from_secs(2),
        sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(pool),
    )
    .await
    .expect("cancelled database future stranded the only pool connection")
    .unwrap();
    assert_ne!(
        pid, old_pid,
        "in-flight SQL connection was returned to the pool"
    );
}

async fn dropped_request(pool: PgPool, operation: &str) {
    let mut f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let (single, pid) = single_connection(&mut f).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let mut pending = Box::pin(async {
        match operation {
            "intake" => katwork::pipeline::submit(
                &f.core,
                &f.tenant,
                f.thread,
                katwork::chat::UserPost {
                    body: "pending".into(),
                    client_msg_id: None,
                },
            )
            .await
            .map(|_| ()),
            "cancel" => {
                katwork::jobs::job::cancel(
                    &single,
                    &f.core.events,
                    &f.tenant,
                    job,
                    std::time::Duration::from_secs(5),
                )
                .await
            }
            "view" => katwork::jobs::job::view(&single, &f.tenant, job)
                .await
                .map(|_| ()),
            _ => unreachable!(),
        }
    });
    tokio::select! {
        result = &mut pending => panic!("database operation did not wait: {result:?}"),
        () = wait_for_backend_lock(&pool, pid) => {}
    }
    drop(pending);
    assert_replaced(&single, pid).await;
    holder.rollback().await.unwrap();
    assert_eq!(f.status(job).await.0, None);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn dropped_intake_discards_in_flight_connection(pool: PgPool) {
    dropped_request(pool, "intake").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn dropped_cancel_discards_in_flight_connection(pool: PgPool) {
    dropped_request(pool, "cancel").await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn dropped_job_view_discards_in_flight_connection(pool: PgPool) {
    dropped_request(pool, "view").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn serve_shutdown_discards_forced_job_view_connection(pool: PgPool) {
    use tokio::io::AsyncWriteExt;
    let mut f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    let (single, pid) = single_connection(&mut f).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(katwork_server::transport::serve_with_shutdown(
        listener,
        f.app.clone(),
        f.core.config.transport.clone(),
        async {
            stopped.await.unwrap();
        },
    ));
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    client.write_all(format!("GET /api/workspaces/{}/jobs/{job} HTTP/1.1\r\nHost: localhost\r\nCookie: {}\r\nConnection: close\r\n\r\n", f.tenant.workspace_id(), f.cookie).as_bytes()).await.unwrap();
    wait_for_backend_lock(&pool, pid).await;
    stop.send(()).unwrap();
    timeout(Duration::from_secs(15), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_replaced(&single, pid).await;
    holder.rollback().await.unwrap();
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn completed_database_work_reuses_connection(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let (single, pid) = single_connection(&mut f).await;
    let (_, job) = f.submit("complete intake").await;
    katwork::jobs::job::view(&single, &f.tenant, job)
        .await
        .unwrap();
    katwork::jobs::job::cancel(
        &single,
        &f.core.events,
        &f.tenant,
        job,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    let reused: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&single)
        .await
        .unwrap();
    assert_eq!(reused, pid);
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn pool_clean_probe_does_not_reserve_unused_parent_capacity(pool: PgPool) {
    let parent = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let held = parent.acquire().await.unwrap();
    let child = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .parent(parent.clone())
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    assert_eq!(child.size(), 1);
    timeout(Duration::from_secs(5), assert_pool_clean(&child))
        .await
        .expect("pool inspection tried to reserve another test's connection permit");
    drop(held);
    timeout(Duration::from_secs(5), child.close())
        .await
        .unwrap();
    drop(child);
    timeout(Duration::from_secs(5), parent.close())
        .await
        .unwrap();
}

pub(super) async fn assert_pool_clean(pool: &PgPool) {
    // sqlx test pools share permits: inspecting a pool must never grow it to maximum capacity.
    timeout(Duration::from_secs(30), async {
        use sqlx::Connection;
        let mut observer = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .unwrap();
        loop {
            let clean: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid() AND state IS DISTINCT FROM 'idle')")
                .fetch_one(&mut observer).await.unwrap();
            if pool.num_idle() as u32 == pool.size() && clean {
                break;
            }
            tokio::task::yield_now().await;
        }
        observer.close().await.unwrap();
    })
    .await
    .expect("worker shutdown left a busy pool or a backend inside a transaction");
}
