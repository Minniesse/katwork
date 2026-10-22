mod support;

use katwork::auth;
use serde_json::json;
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

async fn workspace(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn space(pool: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, $3, false)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn membership(pool: &PgPool, workspace_id: Uuid, user_id: Uuid, role: &str) {
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, $3) ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role")
        .bind(workspace_id).bind(user_id).bind(role).execute(pool).await.unwrap();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_lists_own_spaces(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    membership(&pool, own, id, "member").await;
    let own_space = space(&pool, own, "Office").await;
    space(&pool, other, "Private").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let response = request(
        &app,
        "GET",
        &format!("/api/workspaces/{own}/spaces"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        body(response).await,
        json!([{"id": own_space, "name": "Office", "policy": {"allow_external": false}}])
    );
    let response = request(&app, "GET", "/api/workspaces", None, Some(&cookie)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        body(response).await,
        json!([{"id": own, "name": "Own", "role": "member", "paused": false}])
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn non_member_gets_404_not_403(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Private").await;
    membership(&pool, own, id, "owner").await;
    space(&pool, other, "Private").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    for workspace_id in [other, Uuid::now_v7()] {
        for (method, suffix, payload) in [
            ("GET", "spaces", None),
            (
                "POST",
                "spaces",
                Some(json!({"name": "Intrusion", "allow_external": true})),
            ),
            ("POST", "spaces", Some(json!({"name": "Missing policy"}))),
            ("PUT", "paused", Some(json!({"paused": true}))),
        ] {
            let response = request(
                &app,
                method,
                &format!("/api/workspaces/{workspace_id}/{suffix}"),
                payload,
                Some(&cookie),
            )
            .await;
            assert_code(response, 404, "not_found").await;
        }
    }
    let unchanged: (bool, i64) = sqlx::query_as("SELECT paused, (SELECT count(*) FROM spaces WHERE workspace_id = $1) FROM workspaces WHERE id = $1")
        .bind(other).fetch_one(&pool).await.unwrap();
    assert_eq!(unchanged, (false, 1));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cannot_create_space_admin_can(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let path = format!("/api/workspaces/{workspace_id}/spaces");
    membership(&pool, workspace_id, id, "member").await;
    assert_code(
        request(
            &app,
            "POST",
            &path,
            Some(json!({"name": "Denied", "allow_external": true})),
            Some(&cookie),
        )
        .await,
        403,
        "forbidden",
    )
    .await;
    for (role, allow_external) in [("admin", false), ("owner", true)] {
        membership(&pool, workspace_id, id, role).await;
        let response = request(
            &app,
            "POST",
            &path,
            Some(json!({"name": role, "allow_external": allow_external})),
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), 201);
        let id: Uuid = serde_json::from_value(body(response).await["id"].clone()).unwrap();
        let stored: (Uuid, String, bool) =
            sqlx::query_as("SELECT workspace_id, name, allow_external FROM spaces WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, (workspace_id, role.into(), allow_external));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM spaces")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_space_without_policy_is_422(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    membership(&pool, workspace_id, id, "owner").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let path = format!("/api/workspaces/{workspace_id}/spaces");
    for payload in [
        json!({"name": "No policy"}),
        json!({"name": "Null policy", "allow_external": null}),
    ] {
        assert_code(
            request(&app, "POST", &path, Some(payload), Some(&cookie)).await,
            422,
            "space_policy_required",
        )
        .await;
    }
    assert_code(
        request(
            &app,
            "POST",
            &path,
            Some(json!({"name": "Wrong type", "allow_external": "false"})),
            Some(&cookie),
        )
        .await,
        400,
        "request_invalid",
    )
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM spaces")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_space_allows_external(pool: PgPool) {
    let config = config(None);
    auth::seed_demo(&pool, &config, false).await.unwrap();
    auth::seed_demo(&pool, &config, false).await.unwrap();
    let spaces: Vec<(String, bool)> = sqlx::query_as("SELECT s.name, s.allow_external FROM spaces s JOIN memberships m ON m.workspace_id = s.workspace_id JOIN users u ON u.id = m.user_id WHERE u.email = 'demo@katwork.local'")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(spaces, vec![("Demo".into(), true)]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cannot_pause_workspace_admin_can(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let path = format!("/api/workspaces/{workspace_id}/paused");
    membership(&pool, workspace_id, id, "member").await;
    assert_code(
        request(
            &app,
            "PUT",
            &path,
            Some(json!({"paused": true})),
            Some(&cookie),
        )
        .await,
        403,
        "forbidden",
    )
    .await;
    for (role, paused) in [("admin", true), ("owner", false)] {
        membership(&pool, workspace_id, id, role).await;
        let response = request(
            &app,
            "PUT",
            &path,
            Some(json!({"paused": paused})),
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(body(response).await, json!({}));
        let stored: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, paused);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_workspace_assigns_only_authenticated_owner(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let other = workspace(&pool, "Private").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let response = request(
        &app,
        "POST",
        "/api/workspaces",
        Some(json!({"name": "New office", "workspace_id": other, "owner": Uuid::now_v7()})),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 201);
    let created: Uuid = serde_json::from_value(body(response).await["id"].clone()).unwrap();
    assert_ne!(created, other);
    let members: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT user_id, role FROM memberships WHERE workspace_id = $1")
            .bind(created)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(members, vec![(id, "owner".into())]);
    assert_eq!(
        body(request(&app, "GET", "/api/workspaces", None, Some(&cookie)).await).await,
        json!([{"id": created, "name": "New office", "role": "owner", "paused": false}])
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn every_tenant_route_requires_authentication(pool: PgPool) {
    let workspace_id = workspace(&pool, "Private").await;
    let app = app(&pool, None);
    for (method, path, payload) in [
        ("GET", "/api/workspaces".into(), None),
        (
            "POST",
            "/api/workspaces".into(),
            Some(json!({"name": "New"})),
        ),
        (
            "GET",
            format!("/api/workspaces/{workspace_id}/spaces"),
            None,
        ),
        (
            "POST",
            format!("/api/workspaces/{workspace_id}/spaces"),
            Some(json!({"name": "New", "allow_external": true})),
        ),
        (
            "PUT",
            format!("/api/workspaces/{workspace_id}/paused"),
            Some(json!({"paused": true})),
        ),
    ] {
        assert_code(
            request(&app, method, &path, payload, None).await,
            401,
            "unauthenticated",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn repositories_resolve_roles_and_scope_space_ids(pool: PgPool) {
    use katwork::{
        ids::{SpaceId, UserId, WorkspaceId},
        tenancy::{self, Role, space as spaces},
    };
    let (id, _) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    let own_space = space(&pool, own, "Office").await;
    let other_space = space(&pool, other, "Private").await;
    for (wire, role) in [
        ("owner", Role::Owner),
        ("admin", Role::Admin),
        ("member", Role::Member),
    ] {
        membership(&pool, own, id, wire).await;
        let tenant = tenancy::resolve(&pool, UserId(id), WorkspaceId(own))
            .await
            .unwrap();
        assert_eq!(tenant.user_id, UserId(id));
        assert_eq!(tenant.workspace_id, WorkspaceId(own));
        assert_eq!(tenant.role, role);
        let found = spaces::get(&pool, &tenant, SpaceId(own_space))
            .await
            .unwrap();
        assert_eq!(found.name, "Office");
        assert!(!found.policy.allow_external);
        for hidden in [other_space, Uuid::now_v7()] {
            assert_eq!(
                spaces::get(&pool, &tenant, SpaceId(hidden))
                    .await
                    .unwrap_err()
                    .code(),
                "not_found"
            );
        }
    }
    assert_eq!(
        tenancy::resolve(&pool, UserId(id), WorkspaceId(other))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn tenant_extractor_reads_workspace_id_by_name(pool: PgPool) {
    use axum::{Json, Router, routing::get};
    use katwork::{runtime::Core, tenancy::Tenant};
    use katwork_server::app::AppState;
    use std::sync::Arc;
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    membership(&pool, own, id, "member").await;
    let cookie = cookie(&login(&app(&pool, None), &password).await);
    let router = Router::new()
        .route(
            "/probe/{space_id}/{workspace_id}/{dot_id}",
            get(|tenant: Tenant| async move {
                Json(json!({"workspace_id": tenant.workspace_id, "user_id": tenant.user_id}))
            }),
        )
        .with_state(AppState {
            core: Arc::new(Core::new(pool.clone(), Arc::new(config(None)))),
        });
    let response = request(
        &router,
        "GET",
        &format!("/probe/{other}/{own}/{other}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        body(response).await,
        json!({"workspace_id": own, "user_id": id})
    );
    assert_code(
        request(
            &router,
            "GET",
            &format!("/probe/{own}/{other}/{own}"),
            None,
            Some(&cookie),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    assert_code(
        request(
            &router,
            "GET",
            &format!("/probe/{own}/invalid/{own}"),
            None,
            Some(&cookie),
        )
        .await,
        400,
        "request_invalid",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn workspace_pause_locks_only_workspace_without_blocking_foreign_keys(pool: PgPool) {
    use katwork::{
        ids::{UserId, WorkspaceId},
        tenancy::{self, workspace as workspaces},
    };
    let (id, _) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    membership(&pool, own, id, "owner").await;
    space(&pool, own, "Office").await;
    let tenant = tenancy::resolve(&pool, UserId(id), WorkspaceId(own))
        .await
        .unwrap();
    let mut locks = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR KEY SHARE")
        .bind(own)
        .execute(&mut *locks)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id = $1 FOR NO KEY UPDATE")
        .bind(other)
        .execute(&mut *locks)
        .await
        .unwrap();
    sqlx::query("LOCK TABLE spaces, dots, jobs, tasks, task_leases, threads IN SHARE MODE")
        .execute(&mut *locks)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        workspaces::set_paused(&pool, &tenant, true),
    )
    .await
    .expect("pause must not wait on foreign-key or child locks")
    .unwrap();
    locks.rollback().await.unwrap();
    let rows: Vec<(Uuid, bool)> = sqlx::query_as("SELECT id, paused FROM workspaces ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![(own, true), (other, false)]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn workspace_creation_rolls_back_without_valid_owner(pool: PgPool) {
    use katwork::{ids::UserId, tenancy::workspace as workspaces};
    assert!(
        workspaces::create(&pool, "Orphan", UserId::new())
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
