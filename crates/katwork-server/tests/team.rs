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
                &format!("spaces/{}/kats", self.space),
                Some(json!({"name": name, "role": role, "persona": "Careful"})),
            )
            .await;
        assert_eq!(response.status(), 201);
        body(response).await
    }

    async fn team(&self, lead: &Value, members: &[Value]) -> Value {
        let response = self.call("POST", &format!("spaces/{}/teams", self.space), Some(json!({"name": "Office Kats", "lead": lead["id"], "members": members.iter().map(|d| &d["id"]).collect::<Vec<_>>()}))).await;
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
    let rows: Vec<(String, bool)> = sqlx::query_as("SELECT d.name, m.is_lead FROM team_members m JOIN kats d ON d.id = m.kat_id JOIN workspaces w ON w.id = m.workspace_id WHERE w.name = 'Demo' ORDER BY m.position")
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
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM teams t JOIN workspaces w ON w.id = t.workspace_id WHERE w.name = 'Demo'",
    )
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
    let count: i64 = sqlx::query_scalar("SELECT count(DISTINCT appearance->>'avatar') FROM kats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn lead_defaults_to_three_concurrent_tasks_specialists_to_one(pool: PgPool) {
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
    for locale in [Locale::En, Locale::Th] {
        let space = space(&pool, f.workspace).await;
        let id = team::create_default(&pool, &tenant, SpaceId(space), locale)
            .await
            .unwrap();
        let view = body(f.call("GET", &format!("teams/{id}"), None).await).await;
        let limits: Vec<_> = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kat| (kat["role"].clone(), kat["max_concurrency"].clone()))
            .collect();
        assert_eq!(
            limits,
            [
                (json!("lead"), json!(3)),
                (json!("researcher"), json!(1)),
                (json!("writer"), json!(1)),
                (json!("analyst"), json!(1)),
                (json!("scheduler"), json!(1)),
                (json!("reviewer"), json!(1)),
            ],
            "{locale:?}"
        );
    }
    f.role(&pool, "member").await;
    assert_eq!(
        f.hire("HR", json!({"custom": "HR"})).await["max_concurrency"],
        1
    );
    let lead = f.hire("Chief", json!("lead")).await;
    assert_eq!(lead["max_concurrency"], 3);
    assert_code(
        f.call(
            "POST",
            &format!("spaces/{}/kats", f.space),
            Some(json!({"name": "Fast", "role": "writer", "persona": "", "max_concurrency": 3})),
        )
        .await,
        400,
        "request_invalid",
    )
    .await;
    let path = format!("kats/{}", id(&lead));
    let patch = json!({"max_concurrency": 5});
    assert_code(
        f.call("PATCH", &path, Some(patch.clone())).await,
        403,
        "forbidden",
    )
    .await;
    f.role(&pool, "admin").await;
    let patched = body(f.call("PATCH", &path, Some(patch)).await).await;
    assert_eq!(patched["max_concurrency"], 5);
    assert_eq!(patched["policy_version"], lead["policy_version"]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn hire_uses_role_template_defaults(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let kat = f.hire("  Research  ", json!("researcher")).await;
    assert_eq!(kat["name"], "Research");
    assert_eq!(kat["persona"], "Careful");
    assert_eq!(kat["tool_allowlist"], json!(["ask_user", "read_thread"]));
    assert_eq!(kat["model_tier"], "standard");
    assert_eq!(kat["autonomy"], "ask_first");
    assert_eq!(kat["status"], "active");
    assert_eq!(kat["max_concurrency"], 1);
    assert_eq!(kat["policy_version"], 1);
    assert_eq!(kat["daily_token_budget"], Value::Null);
    assert_eq!(kat["default_space_id"], json!(f.space));
    assert_eq!(kat["spaces"], json!([f.space]));
    assert!(kat["instructions"].as_str().unwrap().contains("research"));
    let response = f
        .call("GET", &format!("spaces/{}/kats", f.space), None)
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await, json!([kat]));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn hire_custom_kat_with_thai_name(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let kat = f.hire("ผู้ช่วยฝ่ายบุคคล", json!({"custom": "HR"})).await;
    assert_eq!(kat["name"], "ผู้ช่วยฝ่ายบุคคล");
    assert_eq!(kat["role"], json!({"custom": "HR"}));
    assert_eq!(kat["appearance"]["avatar"], "kat");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cannot_change_execution_settings_admin_can(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Lead", json!("lead")).await;
    let path = format!("kats/{}", id(&kat));
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
    let kat = f.hire("Lead", json!("lead")).await;
    for (patch, expected) in [
        (json!({"daily_token_budget":1000}), json!(1000)),
        (json!({"name":"Chief"}), json!(1000)),
        (json!({"daily_token_budget":null}), Value::Null),
        (json!({"daily_token_budget":0}), json!(0)),
    ] {
        let response = f
            .call("PATCH", &format!("kats/{}", id(&kat)), Some(patch))
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
async fn duplicate_kat_name_in_team_is_rejected(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (first, second) in [
        ("Writer", "writer"),
        ("Caf\u{e9}", "CAFE\u{301}"),
        ("ΟΣ", "ος"),
        ("Straße", "STRASSE"),
        ("Ｗｒｉｔｅｒ", "Writer"),
        ("Lead Kat", "Lead\u{a0}Kat"),
        ("Lead Kat", "Lead  Kat"),
        ("Lead Kat", "Lead\u{a0} \u{3000}Kat"),
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
                Some(json!({"name":"Kats", "lead":lead["id"], "members":[duplicate["id"]]})),
            )
            .await;
        assert_code(response, 422, "kat_name_conflict").await;
        let team = f.team(&lead, &[]).await;
        let member_path = format!("teams/{}/members/{}", id(&team["team"]), id(&duplicate));
        assert_code(
            f.call("PUT", &member_path, Some(json!({"is_lead":false})))
                .await,
            422,
            "kat_name_conflict",
        )
        .await;
        let changed = f
            .call(
                "PATCH",
                &format!("kats/{}", id(&duplicate)),
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
                &format!("kats/{}", id(&duplicate)),
                Some(json!({"name":second})),
            )
            .await,
            422,
            "kat_name_conflict",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn granting_and_revoking_space_bumps_policy_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Lead", json!("lead")).await;
    let second = space(&pool, f.workspace).await;
    let path = format!("kats/{}/spaces/{second}", id(&kat));
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
        let kat = body(f.call("GET", &format!("kats/{}", id(&kat)), None).await).await;
        assert_eq!(kat["policy_version"], version);
        assert_eq!(kat["spaces"].as_array().unwrap().len(), grants);
        assert_eq!(kat["default_space_id"], json!(f.space));
    }
    assert_code(
        f.call(
            "DELETE",
            &format!("kats/{}/spaces/{}", id(&kat), f.space),
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
            &format!("spaces/{second}/kats"),
            Some(json!({"name":"Other", "role":"writer", "persona":""})),
        )
        .await,
    )
    .await;
    assert_code(
        f.call(
            "POST",
            &format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Kats", "lead":other["id"], "members":[]})),
        )
        .await,
        422,
        "kat_not_in_space",
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
        "kat_not_in_space",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn every_route_hides_other_workspaces_and_foreign_ids(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Lead", json!("lead")).await;
    let team = f.team(&kat, &[]).await;
    let other = workspace(&pool).await;
    let foreign_space = space(&pool, other).await;
    let kat_id = id(&kat);
    let team_id = id(&team["team"]);
    let routes = [
        ("GET", format!("spaces/{}/kats", f.space), None),
        (
            "POST",
            format!("spaces/{}/kats", f.space),
            Some(json!({"name":"Intruder", "role":"lead", "persona":""})),
        ),
        ("GET", format!("kats/{kat_id}"), None),
        (
            "PATCH",
            format!("kats/{kat_id}"),
            Some(json!({"name":"Intruder"})),
        ),
        ("PUT", format!("kats/{kat_id}/spaces/{}", f.space), None),
        ("DELETE", format!("kats/{kat_id}/spaces/{}", f.space), None),
        ("GET", format!("spaces/{}/teams", f.space), None),
        (
            "POST",
            format!("spaces/{}/teams", f.space),
            Some(json!({"name":"Intruder", "lead":kat["id"], "members":[]})),
        ),
        ("GET", format!("teams/{team_id}"), None),
        (
            "PUT",
            format!("teams/{team_id}/members/{kat_id}"),
            Some(json!({"is_lead":false})),
        ),
        ("DELETE", format!("teams/{team_id}/members/{kat_id}"), None),
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
                &format!("kats/{kat_id}/spaces/{foreign_space}"),
                None,
            )
            .await,
            404,
            "not_found",
        )
        .await;
    }
    for suffix in [
        format!("spaces/{foreign_space}/kats"),
        format!("spaces/{foreign_space}/teams"),
    ] {
        assert_code(f.call("GET", &suffix, None).await, 404, "not_found").await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn validates_names_budgets_and_concurrency(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Lead", json!("lead")).await;
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
                &format!("spaces/{}/kats", f.space),
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
                &format!("kats/{}", id(&kat)),
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
                Some(json!({"name":name, "lead":kat["id"], "members":[]})),
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
            f.call("PATCH", &format!("kats/{}", id(&kat)), Some(patch))
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
    let kat = f.hire("Lead", json!("lead")).await;
    for (method, path, payload) in [
        ("GET", "spaces/bad/kats", None),
        (
            "POST",
            "spaces/bad/kats",
            Some(json!({"name":"Lead", "role":"lead", "persona":""})),
        ),
        ("GET", "kats/bad", None),
        ("PATCH", "kats/bad", Some(json!({"name":"Lead"}))),
        ("PUT", "kats/bad/spaces/bad", None),
        ("DELETE", "kats/bad/spaces/bad", None),
        ("GET", "spaces/bad/teams", None),
        (
            "POST",
            "spaces/bad/teams",
            Some(json!({"name":"Team", "lead":kat["id"], "members":[]})),
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
            f.call("PATCH", &format!("kats/{}", id(&kat)), Some(patch))
                .await,
            403,
            "forbidden",
        )
        .await;
    }
    for method in ["PUT", "DELETE"] {
        assert_code(
            f.call(method, "kats/bad/spaces/bad", None).await,
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
            .filter(|d| d.role == katwork::team::KatRole::Lead)
            .count(),
        1
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM kats WHERE workspace_id=$1")
        .bind(f.workspace)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn config_hash_tracks_canonical_execution_config_not_cosmetics(pool: PgPool) {
    use katwork::team::Kat;
    use sha2::{Digest, Sha256};
    let f = Fixture::new(&pool).await;
    let kat: Kat = serde_json::from_value(f.hire("Lead", json!("lead")).await).unwrap();
    let expected = format!(
        "{{\"autonomy\":\"ask_first\",\"instructions\":{},\"model_tier\":\"standard\",\"persona\":\"Careful\",\"policy_version\":1,\"tool_allowlist\":[\"ask_user\",\"read_thread\"]}}",
        serde_json::to_string(&kat.instructions).unwrap()
    );
    assert_eq!(
        kat.config_hash(),
        format!("{:x}", Sha256::digest(expected.as_bytes()))
    );
    let path = format!("kats/{}", kat.id);
    let cosmetic: Kat = serde_json::from_value(
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
    assert_eq!(kat.config_hash(), cosmetic.config_hash());
    assert_eq!(cosmetic.policy_version, 1);
    let persona: Kat = serde_json::from_value(
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
    let first_path = format!("kats/{}", id(&first));
    let second_path = format!("kats/{}", id(&second));
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
    let kat = f.hire("Lead", json!("lead")).await;
    let path = format!("kats/{}", id(&kat));
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
        team::{kat, roles, team},
        tenancy,
        types::Locale,
    };
    let f = Fixture::new(&pool).await;
    let own_kat = f.hire("Lead", json!("lead")).await;
    let own_team = f.team(&own_kat, &[]).await;
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
    let new = roles::template(&katwork::team::KatRole::Lead, Locale::En);
    let foreign = kat::hire(&pool, &tenant, SpaceId(other_space), new.clone())
        .await
        .unwrap();
    let mut forbidden = new;
    forbidden.tool_allowlist.push("exec".into());
    assert_eq!(
        kat::hire(&pool, &tenant, SpaceId(other_space), forbidden)
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
            Some(json!({"name":"Team", "lead":own_kat["id"], "members":[foreign]})),
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
            format!("kats/{foreign}"),
            Some(json!({"status":"paused"})),
        ),
        ("PUT", format!("kats/{foreign}/spaces/{other_space}"), None),
        (
            "DELETE",
            format!("kats/{foreign}/spaces/{other_space}"),
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

#[sqlx::test(migrations = "../katwork/migrations")]
async fn kat_and_team_names_reject_at_sign(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Writer", json!("writer")).await;
    for at in ["@", "＠", "﹫"] {
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/kats", f.space),
                Some(json!({"name":format!("{at}Writer"), "role":"writer", "persona":""})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
        assert_code(
            f.call(
                "PATCH",
                &format!("kats/{}", id(&kat)),
                Some(json!({"name":format!("AI{at}Writer")})),
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
                Some(json!({"name":format!("{at}Team"), "lead":kat["id"], "members":[]})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
    }
}
