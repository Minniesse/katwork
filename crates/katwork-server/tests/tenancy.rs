#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
mod support;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use axum::{body::Body, http::Request};
use katwork::auth;
use serde_json::json;
use sqlx::PgPool;
use support::*;
use tower::ServiceExt;
use uuid::Uuid;

struct CountingAllocator;

thread_local! {
    static LARGE_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record_allocation(size: usize) {
    if size >= 65_536 {
        let _ = LARGE_ALLOCATIONS.try_with(|count| {
            if let Some(value) = count.get() {
                count.set(Some(value + 1));
            }
        });
    }
}

// SAFETY: Every operation forwards the original pointer and layout to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

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
    for payload in [
        json!({"name": "Missing policy"}),
        json!({"name": "Null policy", "allow_external": null}),
        json!({"name": "Wrong type", "allow_external": "false"}),
        json!({}),
    ] {
        assert_code(
            request(&app, "POST", &path, Some(payload), Some(&cookie)).await,
            403,
            "forbidden",
        )
        .await;
    }
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
    let mut config = config(None);
    config.demo = true;
    auth::seed_demo(&pool, &config, false).await.unwrap();
    auth::seed_demo(&pool, &config, false).await.unwrap();
    let spaces: Vec<(String, String, bool)> = sqlx::query_as("SELECT w.name, s.name, s.allow_external FROM spaces s JOIN workspaces w ON w.id = s.workspace_id JOIN memberships m ON m.workspace_id = s.workspace_id JOIN users u ON u.id = m.user_id WHERE u.email = 'demo@katwork.local' ORDER BY w.name = 'Demo' DESC")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(
        spaces,
        vec![
            ("Demo".into(), "Demo".into(), true),
            ("เดโม".into(), "พื้นที่ทดลอง".into(), true)
        ]
    );
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
async fn member_cannot_pause_workspace_with_invalid_body(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    membership(&pool, workspace_id, id, "member").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let path = format!("/api/workspaces/{workspace_id}/paused");
    for payload in [
        Some(json!({})),
        Some(json!({"paused": "yes"})),
        Some(json!({"paused": null})),
        None,
    ] {
        assert_code(
            request(&app, "PUT", &path, payload, Some(&cookie)).await,
            403,
            "forbidden",
        )
        .await;
    }
    let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!paused);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_write_bodies_are_not_deserialized(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let payloads = [
        (
            "POST",
            "spaces",
            format!(
                r#"{{"name":"{}","allow_external":false}}"#,
                "a".repeat(123_457)
            ),
            400,
            true,
        ),
        (
            "PUT",
            "paused",
            format!(r#"{{"paused":false,"{}":0}}"#, r#"\u0061"#.repeat(40_000)),
            200,
            true,
        ),
        ("POST", "spaces", "{".to_owned(), 400, false),
        ("PUT", "paused", "{".to_owned(), 400, false),
    ];
    for role in ["member", "admin"] {
        membership(&pool, workspace_id, id, role).await;
        for (method, suffix, payload, admin_status, parses_large_string) in &payloads {
            // Build the request before counting so only handler work is measured.
            let request = Request::builder()
                .method(*method)
                .uri(format!("/api/workspaces/{workspace_id}/{suffix}"))
                .header("host", "localhost")
                .header("origin", "http://localhost:3000")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(payload.clone()))
                .unwrap();
            LARGE_ALLOCATIONS.set(Some(0));
            let response = app.clone().oneshot(request).await.unwrap();
            let allocations = LARGE_ALLOCATIONS.replace(None).unwrap();
            if role == "member" {
                assert_code(response, 403, "forbidden").await;
                assert_eq!(allocations, 0, "member body parsed by {suffix}");
            } else {
                assert_eq!(response.status(), *admin_status);
                if *parses_large_string {
                    assert!(allocations > 0, "admin positive control for {suffix}");
                }
            }
        }
    }
    let stored: (bool, i64) = sqlx::query_as("SELECT paused, (SELECT count(*) FROM spaces WHERE workspace_id = $1) FROM workspaces WHERE id = $1")
        .bind(workspace_id).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, (false, 0));
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
        assert_eq!(tenant.user_id(), UserId(id));
        assert_eq!(tenant.workspace_id(), WorkspaceId(own));
        assert_eq!(tenant.role(), role);
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
async fn repository_space_create_and_workspace_pause_enforce_roles(pool: PgPool) {
    use katwork::{
        ids::{UserId, WorkspaceId},
        tenancy::{self, space as spaces, workspace as workspaces},
        types::SpacePolicy,
    };
    let (id, _) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    for (role, requested_pause, count, expected_pause) in [
        ("member", true, 0, false),
        ("admin", true, 1, true),
        ("owner", false, 2, false),
    ] {
        membership(&pool, workspace_id, id, role).await;
        let tenant = tenancy::resolve(&pool, UserId(id), WorkspaceId(workspace_id))
            .await
            .unwrap();
        let created = spaces::create(
            &pool,
            &tenant,
            role,
            SpacePolicy {
                allow_external: false,
            },
        )
        .await;
        let paused = workspaces::set_paused(&pool, &tenant, requested_pause).await;
        if role == "member" {
            assert_eq!(created.unwrap_err().code(), "forbidden");
            assert_eq!(paused.unwrap_err().code(), "forbidden");
        } else {
            let created = created.unwrap();
            paused.unwrap();
            let stored: (Uuid, String, bool) = sqlx::query_as(
                "SELECT workspace_id, name, allow_external FROM spaces WHERE id = $1",
            )
            .bind(created)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(stored, (workspace_id, role.to_owned(), false));
        }
        let stored: (bool, i64) = sqlx::query_as("SELECT paused, (SELECT count(*) FROM spaces WHERE workspace_id = $1) FROM workspaces WHERE id = $1")
            .bind(workspace_id).fetch_one(&pool).await.unwrap();
        assert_eq!(stored, (expected_pause, count), "{role}");
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn space_reads_preserve_both_policy_values(pool: PgPool) {
    use katwork::{
        ids::{UserId, WorkspaceId},
        tenancy::{self, space as spaces},
        types::SpacePolicy,
    };
    let (id, password) = user(&pool).await;
    let workspace_id = workspace(&pool, "Office").await;
    membership(&pool, workspace_id, id, "owner").await;
    let tenant = tenancy::resolve(&pool, UserId(id), WorkspaceId(workspace_id))
        .await
        .unwrap();
    let mut expected = Vec::new();
    for (name, allow_external) in [("Local", false), ("Hosted", true)] {
        let id = spaces::create(&pool, &tenant, name, SpacePolicy { allow_external })
            .await
            .unwrap();
        let value = json!({"id": id, "name": name, "policy": {"allow_external": allow_external}});
        let found = spaces::get(&pool, &tenant, id).await.unwrap();
        assert_eq!(serde_json::to_value(found).unwrap(), value);
        expected.push(value);
    }
    let expected = json!(expected);
    assert_eq!(
        serde_json::to_value(spaces::list(&pool, &tenant).await.unwrap()).unwrap(),
        expected
    );
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let response = request(
        &app,
        "GET",
        &format!("/api/workspaces/{workspace_id}/spaces"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await, expected);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn workspace_list_preserves_each_membership_role_and_paused_state(pool: PgPool) {
    use katwork::{ids::UserId, tenancy::workspace as workspaces};
    let (id, password) = user(&pool).await;
    let mut expected = Vec::new();
    for (name, role, paused) in [
        ("Owned", "owner", true),
        ("Managed", "admin", false),
        ("Joined", "member", true),
    ] {
        let workspace_id = workspace(&pool, name).await;
        membership(&pool, workspace_id, id, role).await;
        sqlx::query("UPDATE workspaces SET paused = $2 WHERE id = $1")
            .bind(workspace_id)
            .bind(paused)
            .execute(&pool)
            .await
            .unwrap();
        expected.push(json!({"id": workspace_id, "name": name, "role": role, "paused": paused}));
    }
    workspace(&pool, "Private").await;
    let expected = json!(expected);
    assert_eq!(
        serde_json::to_value(workspaces::list_for(&pool, UserId(id)).await.unwrap()).unwrap(),
        expected
    );
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let response = request(&app, "GET", "/api/workspaces", None, Some(&cookie)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await, expected);
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
            "/probe/{space_id}/{workspace_id}/{kat_id}",
            get(|tenant: Tenant| async move {
                Json(json!({"workspace_id": tenant.workspace_id(), "user_id": tenant.user_id()}))
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
    sqlx::query("LOCK TABLE spaces, kats, jobs, tasks, task_leases, threads IN SHARE MODE")
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

async fn assert_invalid_names(pool: &PgPool, names: &[&str]) {
    let (id, password) = user(pool).await;
    let own = workspace(pool, "Own").await;
    membership(pool, own, id, "owner").await;
    let app = app(pool, None);
    let cookie = cookie(&login(&app, &password).await);
    for name in names {
        for path in [
            "/api/workspaces".to_owned(),
            format!("/api/workspaces/{own}/spaces"),
        ] {
            let response = request(
                &app,
                "POST",
                &path,
                Some(json!({"name": name, "allow_external": false})),
                Some(&cookie),
            )
            .await;
            assert_eq!(response.status(), 400, "name {name:?} on {path}");
            assert_eq!(body(response).await, json!({"code": "request_invalid"}));
        }
    }
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM workspaces), (SELECT count(*) FROM memberships), (SELECT count(*) FROM spaces)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 0));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn empty_names_are_rejected(pool: PgPool) {
    assert_invalid_names(&pool, &[""]).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn whitespace_only_names_are_rejected(pool: PgPool) {
    assert_invalid_names(&pool, &["  \u{2003}  "]).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn names_over_120_characters_are_rejected(pool: PgPool) {
    assert_invalid_names(&pool, &[&"界".repeat(121)]).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn nul_names_are_rejected(pool: PgPool) {
    assert_invalid_names(&pool, &["Office\0"]).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn control_characters_in_names_are_rejected(pool: PgPool) {
    assert_invalid_names(&pool, &["\nOffice\u{0085}"]).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn format_characters_in_names_are_rejected(pool: PgPool) {
    assert_invalid_names(
        &pool,
        &[
            "\u{200B}",
            "\u{FEFF}",
            "Off\u{200B}ice",
            "Off\u{FEFF}ice",
            "Off\u{00AD}ice",
            "Off\u{061C}ice",
            "Off\u{200C}ice",
            "Off\u{200D}ice",
            "Off\u{2060}ice",
            "Off\u{E0001}ice",
            "Off\u{E0020}ice",
        ],
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn bidi_controls_in_names_are_rejected(pool: PgPool) {
    assert_invalid_names(
        &pool,
        &[
            "abc\u{202A}gpj.exe",
            "abc\u{202B}gpj.exe",
            "abc\u{202C}gpj.exe",
            "abc\u{202D}gpj.exe",
            "abc\u{202E}gpj.exe",
            "abc\u{2066}gpj.exe",
            "abc\u{2067}gpj.exe",
            "abc\u{2068}gpj.exe",
            "abc\u{2069}gpj.exe",
        ],
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn line_and_paragraph_separators_in_names_are_rejected(pool: PgPool) {
    assert_invalid_names(
        &pool,
        &[
            "\u{2028}Office",
            "Office\u{2028}",
            "Off\u{2028}ice",
            "\u{2029}Office",
            "Office\u{2029}",
            "Off\u{2029}ice",
        ],
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn names_are_trimmed_and_allow_120_unicode_characters(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    membership(&pool, own, id, "owner").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let name = "界".repeat(120);
    for path in [
        "/api/workspaces".to_owned(),
        format!("/api/workspaces/{own}/spaces"),
    ] {
        let response = request(
            &app,
            "POST",
            &path,
            Some(json!({"name": format!("  {name}\u{2003} "), "allow_external": false})),
            Some(&cookie),
        )
        .await;
        assert_eq!(response.status(), 201);
        let created = body(response).await["id"].clone();
        let response = request(&app, "GET", &path, None, Some(&cookie)).await;
        assert_eq!(response.status(), 200);
        let rows = body(response).await;
        let stored = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == created)
            .unwrap();
        assert_eq!(stored["name"], name);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn removed_membership_denies_every_tenant_route_on_next_request(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    membership(&pool, own, id, "admin").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    for removed in [false, true] {
        if removed {
            sqlx::query("DELETE FROM memberships WHERE workspace_id = $1 AND user_id = $2")
                .bind(own)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        for (method, suffix, payload, status) in [
            ("GET", "spaces", None, 200),
            (
                "POST",
                "spaces",
                Some(json!({"name": "Office", "allow_external": false})),
                201,
            ),
            ("PUT", "paused", Some(json!({"paused": !removed})), 200),
        ] {
            let response = request(
                &app,
                method,
                &format!("/api/workspaces/{own}/{suffix}"),
                payload,
                Some(&cookie),
            )
            .await;
            if removed {
                assert_code(response, 404, "not_found").await;
            } else {
                assert_eq!(response.status(), status);
            }
        }
    }
    let stored: (bool, i64) = sqlx::query_as("SELECT paused, (SELECT count(*) FROM spaces WHERE workspace_id = $1) FROM workspaces WHERE id = $1")
        .bind(own).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, (true, 1));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn downgraded_admin_cannot_write_on_next_request(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    for role in ["admin", "member"] {
        membership(&pool, own, id, role).await;
        for (method, suffix, payload, status) in [
            (
                "POST",
                "spaces",
                json!({"name": "Office", "allow_external": false}),
                201,
            ),
            ("PUT", "paused", json!({"paused": role == "admin"}), 200),
        ] {
            let response = request(
                &app,
                method,
                &format!("/api/workspaces/{own}/{suffix}"),
                Some(payload),
                Some(&cookie),
            )
            .await;
            if role == "member" {
                assert_code(response, 403, "forbidden").await;
            } else {
                assert_eq!(response.status(), status);
            }
        }
    }
    let response = request(
        &app,
        "GET",
        &format!("/api/workspaces/{own}/spaces"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await.as_array().unwrap().len(), 1);
    let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id = $1")
        .bind(own)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(paused);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn admin_role_does_not_carry_to_another_workspace(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    membership(&pool, own, id, "admin").await;
    membership(&pool, other, id, "member").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    for workspace_id in [own, other] {
        for (method, suffix, payload, status) in [
            (
                "POST",
                "spaces",
                json!({"name": "Office", "allow_external": false}),
                201,
            ),
            ("PUT", "paused", json!({"paused": true}), 200),
        ] {
            let response = request(
                &app,
                method,
                &format!("/api/workspaces/{workspace_id}/{suffix}"),
                Some(payload),
                Some(&cookie),
            )
            .await;
            if workspace_id == other {
                assert_code(response, 403, "forbidden").await;
            } else {
                assert_eq!(response.status(), status);
            }
        }
    }
    let stored: (bool, i64) = sqlx::query_as("SELECT paused, (SELECT count(*) FROM spaces WHERE workspace_id = $1) FROM workspaces WHERE id = $1")
        .bind(other).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, (false, 0));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn tenant_extractor_rejects_missing_or_misnamed_capture(pool: PgPool) {
    use axum::{Router, routing::get};
    use katwork::{runtime::Core, tenancy::Tenant};
    use katwork_server::app::AppState;
    use std::sync::Arc;
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    membership(&pool, own, id, "owner").await;
    let cookie = cookie(&login(&app(&pool, None), &password).await);
    for (route, path) in [
        ("/probe", "/probe".to_owned()),
        ("/probe/{id}", format!("/probe/{own}")),
        ("/probe/{workspaceId}", format!("/probe/{own}")),
    ] {
        let router = Router::new()
            .route(route, get(|_: Tenant| async { "authorized" }))
            .with_state(AppState {
                core: Arc::new(Core::new(pool.clone(), Arc::new(config(None)))),
            });
        assert_code(
            request(&router, "GET", &path, None, Some(&cookie)).await,
            400,
            "request_invalid",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn workspace_id_in_body_cannot_override_path(pool: PgPool) {
    let (id, password) = user(&pool).await;
    let own = workspace(&pool, "Own").await;
    let other = workspace(&pool, "Other").await;
    membership(&pool, own, id, "owner").await;
    let app = app(&pool, None);
    let cookie = cookie(&login(&app, &password).await);
    let response = request(
        &app,
        "POST",
        &format!("/api/workspaces/{own}/spaces"),
        Some(json!({"name": "Office", "allow_external": false, "workspace_id": other})),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 201);
    let created: Uuid = serde_json::from_value(body(response).await["id"].clone()).unwrap();
    let stored: Uuid = sqlx::query_scalar("SELECT workspace_id FROM spaces WHERE id = $1")
        .bind(created)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, own);
    let response = request(
        &app,
        "PUT",
        &format!("/api/workspaces/{own}/paused"),
        Some(json!({"paused": true, "workspace_id": other})),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    let rows: Vec<(Uuid, bool)> = sqlx::query_as("SELECT id, paused FROM workspaces ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![(own, true), (other, false)]);
    let other_spaces: i64 =
        sqlx::query_scalar("SELECT count(*) FROM spaces WHERE workspace_id = $1")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(other_spaces, 0);
}
