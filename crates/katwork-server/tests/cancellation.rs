#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[path = "../../katwork/tests/support/cancel.rs"]
mod cancel;
#[allow(dead_code, reason = "Shared HTTP fixture helpers")]
mod support;

use std::{future::Future, sync::Arc, time::Duration};

use axum::{Router, response::Response};
use katwork::{auth, ids::UserId, runtime::Core, tenancy::workspace};
use katwork_server::app::{AppState, build_router};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
use support::*;

async fn single(pool: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

fn app_with_timeout(pool: &PgPool, lock_timeout: Duration) -> Router {
    let mut config = config(None);
    config.lock_timeout = lock_timeout;
    build_router(AppState {
        core: Arc::new(Core::new(pool.clone(), Arc::new(config))),
    })
}

async fn fixture(pool: &PgPool, lock_timeout: Duration) -> (PgPool, Router, String, String, i32) {
    let (user, _) = user(pool).await;
    let workspace = workspace::create(pool, "Workspace", UserId(user))
        .await
        .unwrap();
    let session = auth::session::create(pool, UserId(user)).await.unwrap();
    let cookie = format!("kw_session={}", session.0);
    let single = single(pool).await;
    let app = app_with_timeout(&single, lock_timeout);
    let pid = cancel::backend_pid(&single).await;
    (single, app, cookie, workspace.to_string(), pid)
}

async fn drop_waiting(pool: &PgPool, pid: i32, future: impl Future<Output = Response>) {
    let mut future = Box::pin(future);
    tokio::select! {
        response = future.as_mut() => panic!("request escaped barrier: {}", response.status()),
        () = cancel::wait_for_lock(pool, pid) => {},
    }
    cancel::poll_once_then_drop(future).await;
}

async fn assert_closed(observer: &PgPool, single: &PgPool, pid: i32) {
    assert_ne!(
        cancel::backend_pid(single).await,
        pid,
        "cancelled request reused its backend"
    );
    cancel::assert_backend_idle(observer, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_auth_transaction_closes_connection(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let single = single(&pool).await;
    let app = app_with_timeout(&single, cancel::DEADLINE * 2);
    let pid = cancel::backend_pid(&single).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(&pool, pid, login(&app, &password)).await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_workspace_lock_closes_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let path = format!(
        "/api/workspaces/{workspace}/spaces/{}/kats",
        uuid::Uuid::now_v7()
    );
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "POST",
            &path,
            Some(json!({"name":"Researcher","role":"researcher","persona":""})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_tenancy_create_closes_connection(pool: PgPool) {
    let (single, app, cookie, _, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    // Table locks park the INSERT while allowing authentication's SELECTs.
    sqlx::query("LOCK workspaces IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "POST",
            "/api/workspaces",
            Some(json!({"name":"Cancelled"})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
    let created: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces WHERE name='Cancelled')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!created);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_single_statement_closes_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK spaces IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let path = format!("/api/workspaces/{workspace}/spaces");
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "POST",
            &path,
            Some(json!({"name":"Cancelled","allow_external":false})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_validation_reuses_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    // Validation in message intake happens after BEGIN, exercising the guard's error path.
    let message_path = format!(
        "/api/workspaces/{workspace}/threads/{}/messages",
        uuid::Uuid::now_v7()
    );
    assert_code(
        request(
            &app,
            "POST",
            &message_path,
            Some(json!({"body":"","client_msg_id":"cancel-test"})),
            Some(&cookie),
        )
        .await,
        422,
        "message_empty",
    )
    .await;
    assert_eq!(
        cancel::backend_pid(&single).await,
        pid,
        "422 burned a healthy connection"
    );
    cancel::assert_backend_idle(&pool, pid).await;
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_busy_reuses_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, Duration::from_millis(100)).await;
    let path = format!("/api/workspaces/{workspace}/kats/{}", uuid::Uuid::now_v7());
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let response = tokio::time::timeout(
        cancel::DEADLINE,
        request(
            &app,
            "PATCH",
            &path,
            Some(json!({"name":"Changed"})),
            Some(&cookie),
        ),
    )
    .await
    .expect("unbounded workspace lock");
    assert_eq!(response.headers().get("retry-after").unwrap(), "1");
    assert_code(response, 503, "busy").await;
    assert_eq!(
        cancel::backend_pid(&single).await,
        pid,
        "503 burned a healthy connection"
    );
    cancel::assert_backend_idle(&pool, pid).await;
    blocker.rollback().await.unwrap();
    single.close().await;
}

fn member_change(
    workspace: &str,
    operation: &str,
) -> (&'static str, String, Option<serde_json::Value>) {
    let path = format!("/api/workspaces/{workspace}/members");
    let user = uuid::Uuid::now_v7();
    match operation {
        "invite" => (
            "POST",
            path,
            Some(json!({"email":"cancelled@example.test","role":"member","delivery":"link"})),
        ),
        "role" => (
            "PUT",
            format!("{path}/{user}/role"),
            Some(json!({"role":"admin"})),
        ),
        "remove" => ("DELETE", format!("{path}/{user}"), None),
        _ => unreachable!(),
    }
}

async fn cancelled_member_change(pool: PgPool, operation: &str) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let (method, path, body) = member_change(&workspace, operation);
    drop_waiting(
        &pool,
        pid,
        request(&app, method, &path, body, Some(&cookie)),
    )
    .await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_member_invite_closes_connection(pool: PgPool) {
    cancelled_member_change(pool, "invite").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_member_role_closes_connection(pool: PgPool) {
    cancelled_member_change(pool, "role").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_member_remove_closes_connection(pool: PgPool) {
    cancelled_member_change(pool, "remove").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_member_busy_reuses_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, Duration::from_millis(100)).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    for operation in ["invite", "role", "remove"] {
        let (method, path, body) = member_change(&workspace, operation);
        let response = tokio::time::timeout(
            cancel::DEADLINE,
            request(&app, method, &path, body, Some(&cookie)),
        )
        .await
        .expect("unbounded membership lock");
        assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        assert_code(response, 503, "busy").await;
        assert_eq!(cancel::backend_pid(&single).await, pid);
        cancel::assert_backend_idle(&pool, pid).await;
    }
    blocker.rollback().await.unwrap();
    single.close().await;
}

async fn update_busy_reuses_connection(pool: PgPool, logout: bool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, Duration::from_millis(100)).await;
    let mut blocker = pool.begin().await.unwrap();
    let (method, path, body) = if logout {
        sqlx::query("SELECT id FROM sessions FOR NO KEY UPDATE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        ("POST", "/api/auth/logout".to_owned(), None)
    } else {
        sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
            .bind(&workspace)
            .execute(&mut *blocker)
            .await
            .unwrap();
        (
            "PUT",
            format!("/api/workspaces/{workspace}/paused"),
            Some(json!({"paused":true})),
        )
    };
    let response = tokio::time::timeout(
        cancel::DEADLINE,
        request(&app, method, &path, body, Some(&cookie)),
    )
    .await
    .expect("unbounded UPDATE lock");
    assert_eq!(response.headers().get("retry-after").unwrap(), "1");
    assert_code(response, 503, "busy").await;
    assert_eq!(cancel::backend_pid(&single).await, pid);
    cancel::assert_backend_idle(&pool, pid).await;
    blocker.rollback().await.unwrap();
    single.close().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_pause_busy_reuses_connection(pool: PgPool) {
    update_busy_reuses_connection(pool, false).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_logout_busy_reuses_connection(pool: PgPool) {
    update_busy_reuses_connection(pool, true).await;
}
