// Its own test binary: it holds every Argon2 work slot of the process, which would stall the
// hashing of any test running beside it.
use std::sync::Arc;

use katwork::{
    auth::{mailer::CapturingMailer, password},
    config::Config,
    ids::{UserId, WorkspaceId},
    tenancy::{
        self, Role,
        members::{self, Invite, InviteDelivery},
    },
};
use sqlx::PgPool;
use uuid::Uuid;

fn config() -> Config {
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap()
}

fn invite(email: &str) -> Invite {
    Invite {
        email: email.into(),
        role: Role::Member,
        delivery: InviteDelivery::Link,
    }
}

/// Takes the workspace row the way `guards::authorize` does for every tool call: `FOR SHARE`
/// under a short lock timeout. False when the row stayed locked past it.
async fn tool_call_can_lock(pool: &PgPool, workspace: WorkspaceId) -> bool {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('lock_timeout', '200ms', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let locked = sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR SHARE")
        .bind(workspace)
        .execute(&mut *tx)
        .await;
    match locked {
        Ok(_) => true,
        Err(error)
            if error.as_database_error().and_then(|e| e.code()).as_deref() == Some("55P03") =>
        {
            false
        }
        Err(error) => panic!("{error}"),
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_hashes_before_locking_the_workspace(pool: PgPool) {
    let ws = WorkspaceId::new();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Office')")
        .bind(ws)
        .execute(&pool)
        .await
        .unwrap();
    let owner = UserId::new();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, display_name, locale)
        VALUES ($1, 'owner@example.test', 'unused', 'Owner', 'en')",
    )
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(ws)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&pool, owner, ws).await.unwrap();
    let mailer = Arc::new(CapturingMailer::default());

    // Every hash slot is taken, as in a login burst, so the invite waits for one until its
    // 5 s limit. All that time a running Kat's tool call must still get the workspace row.
    let held = password::hold_every_work_slot().await;
    let waiting = tokio::spawn({
        let (pool, tenant, mailer) = (pool.clone(), tenant.clone(), mailer.clone());
        async move {
            members::invite(
                &pool,
                &config(),
                mailer,
                &tenant,
                invite("new@example.test"),
            )
            .await
        }
    });
    let mut attempts = 0;
    while !waiting.is_finished() {
        assert!(
            tool_call_can_lock(&pool, ws).await,
            "the invite held the workspace row while it waited to hash"
        );
        attempts += 1;
    }
    assert!(attempts > 0);
    assert_eq!(waiting.await.unwrap().unwrap_err().code(), "auth_busy");
    drop(held);

    let invited = members::invite(
        &pool,
        &config(),
        mailer,
        &tenant,
        invite("new@example.test"),
    )
    .await
    .unwrap();
    assert!(invited.created && invited.link.is_some());
}
