#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures hold SQL locks to expose operator hashing boundaries"
)]
#[path = "../../katwork/tests/support/cancel.rs"]
mod cancel;

use katwork::{
    auth::{self, accounts, password},
    config::Config,
    error::KatError,
    ids::UserId,
    tenancy::Role,
    types::{Locale, SpacePolicy},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{sync::Mutex, time::timeout};
use uuid::Uuid;

// These probes occupy every hash slot, so another probe must not start hashing beside them.
static SERIAL: Mutex<()> = Mutex::const_new(());

fn config() -> Config {
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        "KATWORK_DEMO" => Some("1".into()),
        "KATWORK_DEMO_PASSWORD" => Some(Uuid::now_v7().to_string()),
        "KATWORK_LOCK_TIMEOUT_MS" => Some("20000".into()),
        _ => None,
    })
    .unwrap()
}

fn fixture_hash() -> String {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(1024, 1, 1, None).unwrap(),
    )
    .hash_password(
        Uuid::now_v7().to_string().as_bytes(),
        &SaltString::generate(&mut rand_core::OsRng),
    )
    .unwrap()
    .to_string()
}

async fn single(pool: &PgPool) -> (PgPool, i32) {
    let single = PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let pid = cancel::backend_pid(&single).await;
    (single, pid)
}

async fn create_user(
    pool: &PgPool,
    email: &str,
    workspace: &accounts::WorkspaceRef,
) -> Result<String, KatError> {
    accounts::create_user(
        pool,
        email,
        workspace,
        Role::Owner,
        Some(SpacePolicy {
            allow_external: false,
        }),
        Locale::En,
        &config(),
    )
    .await
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_waits_for_hash_capacity_without_a_transaction(pool: PgPool) {
    let _serial = SERIAL.lock().await;
    let held = password::hold_every_work_slot().await;
    let workspace = accounts::WorkspaceRef::Name("Operators".into());
    let creating = create_user(&pool, "waiting@example.test", &workspace);
    let inspect = async {
        loop {
            let idle_transaction: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND state LIKE 'idle in transaction%')")
                .fetch_one(&pool).await.unwrap();
            assert!(
                !idle_transaction,
                "create-user held a transaction while waiting for a hash slot"
            );
            tokio::task::yield_now().await;
        }
    };
    let result = timeout(cancel::DEADLINE, async {
        tokio::select! {
            result = creating => result,
            () = inspect => unreachable!(),
        }
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().code(), "auth_busy");
    drop(held);
    timeout(
        cancel::DEADLINE,
        create_user(&pool, "waiting@example.test", &workspace),
    )
    .await
    .unwrap()
    .unwrap();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_preserves_an_account_created_concurrently(pool: PgPool) {
    let _serial = SERIAL.lock().await;
    let existing_hash = fixture_hash();
    let (single, pid) = single(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(3000004)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let workspace = accounts::WorkspaceRef::Name("Operators".into());
    let mut creating = Box::pin(create_user(&single, "raced@example.test", &workspace));
    tokio::select! {
        _ = creating.as_mut() => panic!("create-user escaped the workspace barrier"),
        () = cancel::wait_for_lock(&pool, pid) => {},
    }
    let held = password::hold_every_work_slot().await;
    let user = UserId::new();
    sqlx::query("INSERT INTO users (id,email,password_hash,display_name,locale) VALUES ($1,'raced@example.test',$2,'Existing','en')")
        .bind(user).bind(&existing_hash).execute(&pool).await.unwrap();
    blocker.rollback().await.unwrap();
    let result = timeout(cancel::DEADLINE, creating).await.unwrap();
    drop(held);
    assert_eq!(result.unwrap_err().code(), "user_exists");
    let stored: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, existing_hash);
    let additions: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM memberships WHERE user_id=$1) + (SELECT count(*) FROM password_resets WHERE user_id=$1) + (SELECT count(*) FROM workspaces WHERE name='Operators')")
        .bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(additions, 0);
    cancel::assert_backend_idle(&pool, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_does_not_hash_after_taking_its_transaction_lock(pool: PgPool) {
    let _serial = SERIAL.lock().await;
    let config = config();
    timeout(cancel::DEADLINE, auth::seed_demo(&pool, &config, false))
        .await
        .unwrap()
        .unwrap();
    let (single, pid) = single(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(3000003)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut seeding = Box::pin(auth::seed_demo(&single, &config, false));
    tokio::select! {
        _ = seeding.as_mut() => panic!("seed-demo escaped its serialization barrier"),
        () = cancel::wait_for_lock(&pool, pid) => {},
    }
    let held = password::hold_every_work_slot().await;
    blocker.rollback().await.unwrap();
    let result = timeout(cancel::DEADLINE, seeding).await.unwrap();
    drop(held);
    assert_eq!(result.unwrap(), None);
    cancel::assert_backend_idle(&pool, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_rechecks_a_password_changed_after_verification(pool: PgPool) {
    let _serial = SERIAL.lock().await;
    let config = config();
    timeout(cancel::DEADLINE, auth::seed_demo(&pool, &config, false))
        .await
        .unwrap()
        .unwrap();
    let user: UserId = sqlx::query_scalar("SELECT id FROM users WHERE email='demo@katwork.local'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let session = auth::session::create(&pool, user, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let changed_hash = fixture_hash();
    let (single, pid) = single(&pool).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(3000003)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut seeding = Box::pin(auth::seed_demo(&single, &config, false));
    tokio::select! {
        _ = seeding.as_mut() => panic!("seed-demo escaped its serialization barrier"),
        () = cancel::wait_for_lock(&pool, pid) => {},
    }
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1")
        .bind(user)
        .bind(changed_hash)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    timeout(cancel::DEADLINE, seeding).await.unwrap().unwrap();
    let stored: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(password::verify(
        config.demo_password.as_deref().unwrap(),
        &stored
    ));
    assert_eq!(
        auth::session::authenticate(&pool, &session.0)
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
    single.close().await;
}
