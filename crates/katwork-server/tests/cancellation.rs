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
    let workspace = workspace::create(
        pool,
        "Workspace",
        UserId(user),
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    let session = auth::session::create(pool, UserId(user), std::time::Duration::from_secs(5))
        .await
        .unwrap();
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

async fn workspace_change_barrier(pool: PgPool, operation: &str, cancel_request: bool) {
    let timeout = if cancel_request {
        cancel::DEADLINE * 2
    } else {
        Duration::from_millis(100)
    };
    let (single, app, cookie, workspace, pid) = fixture(&pool, timeout).await;
    let id = uuid::Uuid::now_v7();
    let (method, path, body) = match operation {
        "hire" => (
            "POST",
            format!("/api/workspaces/{workspace}/spaces/{id}/kats"),
            json!({"name":"Researcher","role":"researcher","persona":""}),
        ),
        "update" => (
            "PATCH",
            format!("/api/workspaces/{workspace}/kats/{id}"),
            json!({"name":"Changed"}),
        ),
        "add_member" => (
            "PUT",
            format!("/api/workspaces/{workspace}/teams/{id}/members/{id}"),
            json!({"is_lead":false}),
        ),
        _ => unreachable!(),
    };
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pending = request(&app, method, &path, Some(body), Some(&cookie));
    if cancel_request {
        drop_waiting(&pool, pid, pending).await;
        blocker.rollback().await.unwrap();
        assert_closed(&pool, &single, pid).await;
    } else {
        let response = tokio::time::timeout(cancel::DEADLINE, pending)
            .await
            .expect("workspace lock ignored configured timeout");
        assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        assert_code(response, 503, "busy").await;
        assert_eq!(cancel::backend_pid(&single).await, pid);
        cancel::assert_backend_idle(&pool, pid).await;
        blocker.rollback().await.unwrap();
        single.close().await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_kat_update_closes_connection(pool: PgPool) {
    workspace_change_barrier(pool, "update", true).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_team_add_member_closes_connection(pool: PgPool) {
    workspace_change_barrier(pool, "add_member", true).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_kat_hire_busy_reuses_connection(pool: PgPool) {
    workspace_change_barrier(pool, "hire", false).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_team_add_member_busy_reuses_connection(pool: PgPool) {
    workspace_change_barrier(pool, "add_member", false).await;
}

async fn auth_barrier(pool: PgPool, operation: &str, cancel_request: bool) {
    let (user, password) = user(&pool).await;
    let (path, body) = if operation == "login" {
        (
            "/api/auth/login".to_owned(),
            json!({"email":"person@example.test","password":password}),
        )
    } else {
        let mut conn = pool.acquire().await.unwrap();
        let token = if operation == "reset" {
            auth::reset::issue(&mut conn, UserId(user)).await.unwrap()
        } else {
            auth::invite::issue(&mut conn, UserId(user)).await.unwrap()
        };
        (
            format!(
                "/api/auth/{operation}/{}",
                if operation == "reset" {
                    "confirm"
                } else {
                    "accept"
                }
            ),
            json!({"token":token,"new_password":uuid::Uuid::now_v7().to_string()}),
        )
    };
    let single = single(&pool).await;
    let app = app_with_timeout(
        &single,
        if cancel_request {
            cancel::DEADLINE * 2
        } else {
            Duration::from_millis(100)
        },
    );
    let pid = cancel::backend_pid(&single).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(user)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pending = request(&app, "POST", &path, Some(body), None);
    if cancel_request {
        drop_waiting(&pool, pid, pending).await;
        blocker.rollback().await.unwrap();
        assert_closed(&pool, &single, pid).await;
        let consumed: i64 =
            sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE used_at IS NOT NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(consumed, 0);
    } else {
        let response = tokio::time::timeout(cancel::DEADLINE, pending)
            .await
            .expect("user lock ignored configured timeout");
        assert_eq!(response.headers().get("retry-after").unwrap(), "1");
        assert_code(response, 503, "busy").await;
        assert_eq!(cancel::backend_pid(&single).await, pid);
        cancel::assert_backend_idle(&pool, pid).await;
        blocker.rollback().await.unwrap();
        single.close().await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_reset_confirm_closes_connection(pool: PgPool) {
    auth_barrier(pool, "reset", true).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_invite_accept_closes_connection(pool: PgPool) {
    auth_barrier(pool, "invite", true).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_login_busy_reuses_connection(pool: PgPool) {
    auth_barrier(pool, "login", false).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_reset_confirm_busy_reuses_connection(pool: PgPool) {
    auth_barrier(pool, "reset", false).await;
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn normal_invite_accept_busy_reuses_connection(pool: PgPool) {
    auth_barrier(pool, "invite", false).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_thread_create_closes_connection(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("LOCK threads IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(&pool, pid, request(&app, "POST", &format!("/api/workspaces/{workspace}/threads"), Some(json!({"space_id":uuid::Uuid::now_v7(),"team_id":uuid::Uuid::now_v7(),"title":"Cancelled"})), Some(&cookie))).await;
    blocker.rollback().await.unwrap();
    assert_closed(&pool, &single, pid).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cancelled_pause_does_not_apply_after_unlock(pool: PgPool) {
    let (single, app, cookie, workspace, pid) = fixture(&pool, cancel::DEADLINE * 2).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id::text=$1 FOR NO KEY UPDATE")
        .bind(&workspace)
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop_waiting(
        &pool,
        pid,
        request(
            &app,
            "PUT",
            &format!("/api/workspaces/{workspace}/paused"),
            Some(json!({"paused":true})),
            Some(&cookie),
        ),
    )
    .await;
    blocker.rollback().await.unwrap();
    cancel::assert_backend_idle(&pool, pid).await;
    let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id::text=$1")
        .bind(&workspace)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !paused,
        "dropped UPDATE committed after its row lock was released"
    );
    assert_closed(&pool, &single, pid).await;
}
