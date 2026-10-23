#[path = "team/concurrency.rs"]
mod concurrency;
#[path = "team/coverage.rs"]
mod coverage;
#[path = "team/grants.rs"]
mod grants;
#[path = "team/names.rs"]
mod names;
#[path = "team/readers.rs"]
mod readers;
mod support;
#[path = "team/validation.rs"]
mod validation;

use axum::{Router, response::Response};
use katwork::auth;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

struct Fixture {
    app: Router,
    cookie: String,
    user: Uuid,
    workspace: Uuid,
    space: Uuid,
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let (user, password) = user(pool).await;
        let workspace = workspace(pool).await;
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
        )
        .bind(workspace)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
        let space = space(pool, workspace).await;
        let app = app(pool, None);
        let cookie = cookie(&login(&app, &password).await);
        Self {
            app,
            cookie,
            user,
            workspace,
            space,
        }
    }

    async fn call(&self, method: &str, suffix: &str, payload: Option<Value>) -> Response {
        request(
            &self.app,
            method,
            &format!("/api/workspaces/{}/{suffix}", self.workspace),
            payload,
            Some(&self.cookie),
        )
        .await
    }

    async fn hire(&self, name: &str, role: Value) -> Value {
        let response = self
            .call(
                "POST",
                &format!("spaces/{}/dots", self.space),
                Some(json!({"name": name, "role": role, "persona": "Careful"})),
            )
            .await;
        assert_eq!(response.status(), 201);
        body(response).await
    }

    async fn team(&self, lead: &Value, members: &[Value]) -> Value {
        let response = self.call("POST", &format!("spaces/{}/teams", self.space), Some(json!({"name": "Office Dots", "lead": lead["id"], "members": members.iter().map(|d| &d["id"]).collect::<Vec<_>>()}))).await;
        assert_eq!(response.status(), 201);
        body(response).await
    }

    async fn role(&self, pool: &PgPool, role: &str) {
        sqlx::query("UPDATE memberships SET role = $3 WHERE workspace_id = $1 AND user_id = $2")
            .bind(self.workspace)
            .bind(self.user)
            .bind(role)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn workspace(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Office')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn space(pool: &PgPool, workspace: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Office', false)")
        .bind(id).bind(workspace).execute(pool).await.unwrap();
    id
}

fn id(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn default_team_has_one_lead_and_five_specialists(pool: PgPool) {
    let mut config = config(None);
    config.demo = true;
    auth::seed_demo(&pool, &config, false).await.unwrap();
    auth::seed_demo(&pool, &config, false).await.unwrap();
    let rows: Vec<(String, bool)> = sqlx::query_as("SELECT d.name, m.is_lead FROM team_members m JOIN dots d ON d.id = m.dot_id ORDER BY m.position")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(
        rows,
        vec![
            ("Lead".into(), true),
            ("Researcher".into(), false),
            ("Writer".into(), false),
            ("Analyst".into(), false),
            ("Scheduler".into(), false),
            ("Reviewer".into(), false)
        ]
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM teams")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn default_roster_avatars_are_distinct(pool: PgPool) {
    let mut config = config(None);
    config.demo = true;
    auth::seed_demo(&pool, &config, false).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(DISTINCT appearance->>'avatar') FROM dots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn hire_uses_role_template_defaults(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let dot = f.hire("  Research  ", json!("researcher")).await;
    assert_eq!(dot["name"], "Research");
    assert_eq!(dot["persona"], "Careful");
    assert_eq!(dot["tool_allowlist"], json!(["ask_user", "read_thread"]));
    assert_eq!(dot["model_tier"], "standard");
    assert_eq!(dot["autonomy"], "ask_first");
    assert_eq!(dot["status"], "active");
    assert_eq!(dot["max_concurrency"], 1);
    assert_eq!(dot["policy_version"], 1);
    assert_eq!(dot["daily_token_budget"], Value::Null);
    assert_eq!(dot["default_space_id"], json!(f.space));
    assert_eq!(dot["spaces"], json!([f.space]));
    assert!(dot["instructions"].as_str().unwrap().contains("research"));
    let response = f
        .call("GET", &format!("spaces/{}/dots", f.space), None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await, json!([dot]));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn hire_custom_dot_with_thai_name(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let dot = f.hire("ผู้ช่วยฝ่ายบุคคล", json!({"custom": "HR"})).await;
    assert_eq!(dot["name"], "ผู้ช่วยฝ่ายบุคคล");
    assert_eq!(dot["role"], json!({"custom": "HR"}));
    assert_eq!(dot["appearance"]["avatar"], "dot");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cannot_change_execution_settings_admin_can(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&dot));
    let patches = [
        json!({"instructions":"Be concise"}),
        json!({"tool_allowlist":["read_thread"]}),
        json!({"model_tier":"strong"}),
        json!({"autonomy":"standard"}),
        json!({"status":"paused"}),
        json!({"max_concurrency":2}),
        json!({"daily_token_budget":100}),
        json!({"daily_token_budget":null}),
    ];
    f.role(&pool, "member").await;
    for patch in &patches {
        assert_code(
            f.call("PATCH", &path, Some(patch.clone())).await,
            403,
            "forbidden",
        )
        .await;
    }
    let response = f.call("PATCH", &path, Some(json!({"name":"Chief", "persona":"Patient", "appearance":{"avatar":"pen", "accent":"blue"}}))).await;
    assert_eq!(response.status(), 200);
    let before = body(response).await;
    f.role(&pool, "admin").await;
    let mut version = before["policy_version"].as_i64().unwrap();
    for (patch, bump) in patches.into_iter().zip([0, 1, 1, 1, 1, 0, 0, 0]) {
        let response = f.call("PATCH", &path, Some(patch.clone())).await;
        assert_eq!(response.status(), 200);
        version += bump;
        assert_eq!(body(response).await["policy_version"], version);
        let same = f.call("PATCH", &path, Some(patch)).await;
        assert_eq!(same.status(), 200);
        assert_eq!(body(same).await["policy_version"], version);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn patch_daily_budget_omitted_null_and_number(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    for (patch, expected) in [
        (json!({"daily_token_budget":1000}), json!(1000)),
        (json!({"name":"Chief"}), json!(1000)),
        (json!({"daily_token_budget":null}), Value::Null),
        (json!({"daily_token_budget":0}), json!(0)),
    ] {
        let response = f
            .call("PATCH", &format!("dots/{}", id(&dot)), Some(patch))
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(body(response).await["daily_token_budget"], expected);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn team_rejects_second_lead(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let writer = f.hire("Writer", json!("writer")).await;
    let team = f.team(&lead, std::slice::from_ref(&writer)).await;
    let prefix = format!("teams/{}/members", id(&team["team"]));
    assert_code(
        f.call(
            "PUT",
            &format!("{prefix}/{}", id(&writer)),
            Some(json!({"is_lead":true})),
        )
        .await,
        422,
        "team_lead_conflict",
    )
    .await;
    assert_code(
        f.call(
            "PUT",
            &format!("{prefix}/{}", id(&lead)),
            Some(json!({"is_lead":false})),
        )
        .await,
        422,
        "team_lead_required",
    )
    .await;
    assert_code(
        f.call("DELETE", &format!("{prefix}/{}", id(&lead)), None)
            .await,
        422,
        "team_lead_required",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn team_persisted_server_side_and_returned_in_order(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let writer = f.hire("Writer", json!("writer")).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let researcher = f.hire("Researcher", json!("researcher")).await;
    let team = f.team(&lead, &[researcher.clone(), writer.clone()]).await;
    let path = format!("teams/{}", id(&team["team"]));
    let response = request(
        &app(&pool, None),
        "GET",
        &format!("/api/workspaces/{}/{path}", f.workspace),
        None,
        Some(&f.cookie),
    )
    .await;
    assert_eq!(response.status(), 200);
    let view = body(response).await;
    assert_eq!(view["members"], json!([lead, researcher, writer]));
    let list = f
        .call("GET", &format!("spaces/{}/teams", f.space), None)
        .await;
    assert_eq!(body(list).await, json!([team["team"]]));
    let member_path = format!("{path}/members/{}", id(&writer));
    assert_eq!(f.call("DELETE", &member_path, None).await.status(), 204);
    assert_eq!(
        body(f.call("GET", &path, None).await).await["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.call("PUT", &member_path, Some(json!({"is_lead":false})))
            .await
            .status(),
        204
    );
    assert_eq!(
        f.call("PUT", &member_path, Some(json!({"is_lead":false})))
            .await
            .status(),
        204
    );
    assert_eq!(
        body(f.call("GET", &path, None).await).await["members"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn duplicate_dot_name_in_team_is_rejected(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (first, second) in [
        ("Writer", "writer"),
        ("Caf\u{e9}", "CAFE\u{301}"),
        ("ΟΣ", "ος"),
        ("Straße", "STRASSE"),
        ("Ｗｒｉｔｅｒ", "Writer"),
        ("Lead Dot", "Lead\u{a0}Dot"),
        ("Lead Dot", "Lead  Dot"),
        ("Lead Dot", "Lead\u{a0} \u{3000}Dot"),
        ("ก่", "ก่่"),
        ("ปี่", "ป\u{e48}\u{e35}\u{e48}"),
        ("Writer\u{1c89}", "Writer\u{1c8a}"),
        ("ปี่", "ป\u{e48}\u{e35}"),
        ("นำ", "น\u{e4d}า"),
        ("น้ำ", "น\u{e4d}\u{e49}า"),
        ("กุ่", "ก\u{e48}\u{e38}"),
    ] {
        let lead = f.hire(first, json!("lead")).await;
        let duplicate = f.hire(second, json!("writer")).await;
        let response = f
            .call(
                "POST",
                &format!("spaces/{}/teams", f.space),
                Some(json!({"name":"Dots", "lead":lead["id"], "members":[duplicate["id"]]})),
            )
            .await;
        assert_code(response, 422, "dot_name_conflict").await;
        let team = f.team(&lead, &[]).await;
        let member_path = format!("teams/{}/members/{}", id(&team["team"]), id(&duplicate));
        assert_code(
            f.call("PUT", &member_path, Some(json!({"is_lead":false})))
                .await,
            422,
            "dot_name_conflict",
        )
        .await;
        let changed = f
            .call(
                "PATCH",
                &format!("dots/{}", id(&duplicate)),
                Some(json!({"name":"Other"})),
            )
            .await;
        assert_eq!(changed.status(), 200);
        assert_eq!(
            f.call("PUT", &member_path, Some(json!({"is_lead":false})))
                .await
                .status(),
            204
        );
        assert_code(
            f.call(
                "PATCH",
                &format!("dots/{}", id(&duplicate)),
                Some(json!({"name":second})),
            )
            .await,
            422,
            "dot_name_conflict",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn granting_and_revoking_space_bumps_policy_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    let second = space(&pool, f.workspace).await;
    let path = format!("dots/{}/spaces/{second}", id(&dot));
    f.role(&pool, "member").await;
    for method in ["PUT", "DELETE"] {
        assert_code(f.call(method, &path, None).await, 403, "forbidden").await;
    }
    f.role(&pool, "admin").await;
    for (method, version, grants) in [
        ("PUT", 2, 2),
        ("PUT", 2, 2),
        ("DELETE", 3, 1),
        ("DELETE", 3, 1),
    ] {
        assert_eq!(f.call(method, &path, None).await.status(), 204);
        let dot = body(f.call("GET", &format!("dots/{}", id(&dot)), None).await).await;
        assert_eq!(dot["policy_version"], version);
        assert_eq!(dot["spaces"].as_array().unwrap().len(), grants);
        assert_eq!(dot["default_space_id"], json!(f.space));
    }
    assert_code(
        f.call(
            "DELETE",
            &format!("dots/{}/spaces/{}", id(&dot), f.space),
            None,
        )
        .await,
        422,
        "default_space_required",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn team_requires_members_granted_to_its_space(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let second = space(&pool, f.workspace).await;
    let other = body(
        f.call(
            "POST",
            &format!("spaces/{second}/dots"),
            Some(json!({"name":"Other", "role":"writer", "persona":""})),
        )
        .await,
    )
    .await;
    assert_code(
        f.call(
            "POST",
            &format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Dots", "lead":other["id"], "members":[]})),
        )
        .await,
        422,
        "dot_not_in_space",
    )
    .await;
    let team = f.team(&lead, &[]).await;
    assert_code(
        f.call(
            "PUT",
            &format!("teams/{}/members/{}", id(&team["team"]), id(&other)),
            Some(json!({"is_lead":false})),
        )
        .await,
        422,
        "dot_not_in_space",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn every_route_hides_other_workspaces_and_foreign_ids(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    let team = f.team(&dot, &[]).await;
    let other = workspace(&pool).await;
    let foreign_space = space(&pool, other).await;
    let dot_id = id(&dot);
    let team_id = id(&team["team"]);
    let routes = [
        ("GET", format!("spaces/{}/dots", f.space), None),
        (
            "POST",
            format!("spaces/{}/dots", f.space),
            Some(json!({"name":"Intruder", "role":"lead", "persona":""})),
        ),
        ("GET", format!("dots/{dot_id}"), None),
        (
            "PATCH",
            format!("dots/{dot_id}"),
            Some(json!({"name":"Intruder"})),
        ),
        ("PUT", format!("dots/{dot_id}/spaces/{}", f.space), None),
        ("DELETE", format!("dots/{dot_id}/spaces/{}", f.space), None),
        ("GET", format!("spaces/{}/teams", f.space), None),
        (
            "POST",
            format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Intruder", "lead":dot["id"], "members":[]})),
        ),
        ("GET", format!("teams/{team_id}"), None),
        (
            "PUT",
            format!("teams/{team_id}/members/{dot_id}"),
            Some(json!({"is_lead":false})),
        ),
        ("DELETE", format!("teams/{team_id}/members/{dot_id}"), None),
    ];
    for member in [false, true] {
        if member {
            sqlx::query(
                "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
            )
            .bind(other)
            .bind(f.user)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (method, suffix, payload) in &routes {
            let response = request(
                &f.app,
                method,
                &format!("/api/workspaces/{other}/{suffix}"),
                payload.clone(),
                Some(&f.cookie),
            )
            .await;
            assert_code(response, 404, "not_found").await;
        }
    }
    for method in ["PUT", "DELETE"] {
        assert_code(
            f.call(
                method,
                &format!("dots/{dot_id}/spaces/{foreign_space}"),
                None,
            )
            .await,
            404,
            "not_found",
        )
        .await;
    }
    for suffix in [
        format!("spaces/{foreign_space}/dots"),
        format!("spaces/{foreign_space}/teams"),
    ] {
        assert_code(f.call("GET", &suffix, None).await, 404, "not_found").await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn validates_names_budgets_and_concurrency(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    for name in [
        "".to_owned(),
        "   ".into(),
        "x\nx".into(),
        "x\0x".into(),
        "x\u{202e}x".into(),
        "x\u{200b}x".into(),
        "x\u{2028}x".into(),
        "x\u{2029}x".into(),
        "ก".repeat(121),
    ] {
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/dots", f.space),
                Some(json!({"name":name, "role":"writer", "persona":""})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
        assert_code(
            f.call(
                "PATCH",
                &format!("dots/{}", id(&dot)),
                Some(json!({"name":name})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/teams", f.space),
                Some(json!({"name":name, "lead":dot["id"], "members":[]})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
    }
    for patch in [
        json!({"daily_token_budget":-1}),
        json!({"max_concurrency":0}),
        json!({"max_concurrency":65536}),
        json!({"default_space_id":Uuid::now_v7()}),
    ] {
        assert_code(
            f.call("PATCH", &format!("dots/{}", id(&dot)), Some(patch))
                .await,
            400,
            "request_invalid",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn malformed_path_ids_use_json_errors_and_execution_keys_check_role_first(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    for (method, path, payload) in [
        ("GET", "spaces/bad/dots", None),
        (
            "POST",
            "spaces/bad/dots",
            Some(json!({"name":"Lead", "role":"lead", "persona":""})),
        ),
        ("GET", "dots/bad", None),
        ("PATCH", "dots/bad", Some(json!({"name":"Lead"}))),
        ("PUT", "dots/bad/spaces/bad", None),
        ("DELETE", "dots/bad/spaces/bad", None),
        ("GET", "spaces/bad/teams", None),
        (
            "POST",
            "spaces/bad/teams",
            Some(json!({"name":"Team", "lead":dot["id"], "members":[]})),
        ),
        ("GET", "teams/bad", None),
        (
            "PUT",
            "teams/bad/members/bad",
            Some(json!({"is_lead":false})),
        ),
        ("DELETE", "teams/bad/members/bad", None),
    ] {
        assert_code(f.call(method, path, payload).await, 400, "request_invalid").await;
    }
    f.role(&pool, "member").await;
    for patch in [
        json!({"instructions":12}),
        json!({"daily_token_budget":"invalid"}),
        json!({"max_concurrency":0}),
        json!({"status":null}),
    ] {
        assert_code(
            f.call("PATCH", &format!("dots/{}", id(&dot)), Some(patch))
                .await,
            403,
            "forbidden",
        )
        .await;
    }
    for method in ["PUT", "DELETE"] {
        assert_code(
            f.call(method, "dots/bad/spaces/bad", None).await,
            403,
            "forbidden",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thai_default_roster_is_atomic_and_idempotent(pool: PgPool) {
    use katwork::{
        ids::{SpaceId, UserId, WorkspaceId},
        team::team,
        tenancy,
        types::Locale,
    };
    let f = Fixture::new(&pool).await;
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let lock = concurrency::lock_workspace(&pool, f.workspace).await;
    let (a, b, ()) = tokio::join!(
        team::create_default(&pool, &tenant, SpaceId(f.space), Locale::Th),
        team::create_default(&pool, &tenant, SpaceId(f.space), Locale::Th),
        async {
            concurrency::wait_for_blocked_writers(&pool, 2).await;
            lock.rollback().await.unwrap();
        }
    );
    let id = a.unwrap();
    assert_eq!(id, b.unwrap());
    let view = team::get(&pool, &tenant, id).await.unwrap();
    assert_eq!(
        view.members
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["หัวหน้าทีม", "นักวิจัย", "นักเขียน", "นักวิเคราะห์", "เลขา", "ผู้ตรวจ"]
    );
    assert_eq!(
        view.members
            .iter()
            .map(|d| &d.appearance.avatar)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        view.members
            .iter()
            .filter(|d| d.role == katwork::team::DotRole::Lead)
            .count(),
        1
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM dots WHERE workspace_id=$1")
        .bind(f.workspace)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn config_hash_tracks_canonical_execution_config_not_cosmetics(pool: PgPool) {
    use katwork::team::Dot;
    use sha2::{Digest, Sha256};
    let f = Fixture::new(&pool).await;
    let dot: Dot = serde_json::from_value(f.hire("Lead", json!("lead")).await).unwrap();
    let expected = format!(
        "{{\"autonomy\":\"ask_first\",\"instructions\":{},\"model_tier\":\"standard\",\"persona\":\"Careful\",\"policy_version\":1,\"tool_allowlist\":[\"ask_user\",\"read_thread\"]}}",
        serde_json::to_string(&dot.instructions).unwrap()
    );
    assert_eq!(
        dot.config_hash(),
        format!("{:x}", Sha256::digest(expected.as_bytes()))
    );
    let path = format!("dots/{}", dot.id);
    let cosmetic: Dot = serde_json::from_value(
        body(
            f.call(
                "PATCH",
                &path,
                Some(json!({"name":"Chief", "appearance":{"avatar":"pen", "accent":"red"}})),
            )
            .await,
        )
        .await,
    )
    .unwrap();
    assert_eq!(dot.config_hash(), cosmetic.config_hash());
    assert_eq!(cosmetic.policy_version, 1);
    let persona: Dot = serde_json::from_value(
        body(
            f.call("PATCH", &path, Some(json!({"persona":"Patient"})))
                .await,
        )
        .await,
    )
    .unwrap();
    assert_ne!(persona.config_hash(), cosmetic.config_hash());
    assert_eq!(persona.policy_version, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_renames_and_memberships_preserve_unique_team_names(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let first = f.hire("First", json!("writer")).await;
    let second = f.hire("Second", json!("writer")).await;
    let team = f.team(&lead, &[first.clone(), second.clone()]).await;
    let first_path = format!("dots/{}", id(&first));
    let second_path = format!("dots/{}", id(&second));
    let (a, b) = tokio::join!(
        f.call("PATCH", &first_path, Some(json!({"name":"Writer"}))),
        f.call("PATCH", &second_path, Some(json!({"name":"writer"})))
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 422]);
    let first = f.hire("Research", json!("researcher")).await;
    let second = f.hire("RESEARCH", json!("researcher")).await;
    let first_path = format!("teams/{}/members/{}", id(&team["team"]), id(&first));
    let second_path = format!("teams/{}/members/{}", id(&team["team"]), id(&second));
    let (a, b) = tokio::join!(
        f.call("PUT", &first_path, Some(json!({"is_lead":false}))),
        f.call("PUT", &second_path, Some(json!({"is_lead":false})))
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [204, 422]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_policy_edits_keep_both_changes_and_versions(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&dot));
    let lock = concurrency::lock_workspace(&pool, f.workspace).await;
    let (a, b, ()) = tokio::join!(
        f.call("PATCH", &path, Some(json!({"status":"paused"}))),
        f.call(
            "PATCH",
            &path,
            Some(json!({"tool_allowlist":["read_thread"]}))
        ),
        async {
            concurrency::wait_for_blocked_writers(&pool, 2).await;
            lock.rollback().await.unwrap();
        }
    );
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    let result = body(f.call("GET", &path, None).await).await;
    assert_eq!(result["status"], "paused");
    assert_eq!(result["tool_allowlist"], json!(["read_thread"]));
    assert_eq!(result["policy_version"], 3);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn foreign_members_and_leads_are_hidden_and_role_is_workspace_scoped(pool: PgPool) {
    use katwork::{
        ids::{SpaceId, UserId, WorkspaceId},
        team::{dot, roles, team},
        tenancy,
        types::Locale,
    };
    let f = Fixture::new(&pool).await;
    let own_dot = f.hire("Lead", json!("lead")).await;
    let own_team = f.team(&own_dot, &[]).await;
    let other = workspace(&pool).await;
    let other_space = space(&pool, other).await;
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1,$2,'member')")
        .bind(other)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(other))
        .await
        .unwrap();
    let new = roles::template(&katwork::team::DotRole::Lead, Locale::En);
    let foreign = dot::hire(&pool, &tenant, SpaceId(other_space), new.clone())
        .await
        .unwrap();
    let mut forbidden = new;
    forbidden.tool_allowlist.push("exec".into());
    assert_eq!(
        dot::hire(&pool, &tenant, SpaceId(other_space), forbidden)
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_code(
        f.call(
            "POST",
            &format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Team", "lead":foreign, "members":[]})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    assert_code(
        f.call(
            "POST",
            &format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Team", "lead":own_dot["id"], "members":[foreign]})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    for (method, payload) in [("PUT", Some(json!({"is_lead":false}))), ("DELETE", None)] {
        assert_code(
            f.call(
                method,
                &format!("teams/{}/members/{foreign}", id(&own_team["team"])),
                payload,
            )
            .await,
            404,
            "not_found",
        )
        .await;
    }
    for (method, suffix, payload) in [
        (
            "PATCH",
            format!("dots/{foreign}"),
            Some(json!({"status":"paused"})),
        ),
        ("PUT", format!("dots/{foreign}/spaces/{other_space}"), None),
        (
            "DELETE",
            format!("dots/{foreign}/spaces/{other_space}"),
            None,
        ),
    ] {
        assert_code(
            request(
                &f.app,
                method,
                &format!("/api/workspaces/{other}/{suffix}"),
                payload,
                Some(&f.cookie),
            )
            .await,
            403,
            "forbidden",
        )
        .await;
    }
    let count = team::list(&pool, &tenant, SpaceId(other_space))
        .await
        .unwrap();
    assert!(count.is_empty());
}
