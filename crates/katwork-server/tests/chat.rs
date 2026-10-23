#[path = "chat/persistence.rs"]
mod persistence;
mod support;

use axum::{Router, body::Body, http::Request, response::Response};
use katwork::{
    chat::{Author, NewMessage, message, thread},
    ids::{JobId, KatId, MessageId, SpaceId, TeamId, ThreadId, UserId, WorkspaceId},
    team::team,
    tenancy::{self, Tenant},
    types::{Locale, SpacePolicy},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    cookie: String,
    tenant: Tenant,
    space: SpaceId,
    team: TeamId,
    thread: ThreadId,
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let (user, password) = user(pool).await;
        let workspace = WorkspaceId::new();
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Chat')")
            .bind(workspace)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'member')",
        )
        .bind(workspace)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
        let tenant = tenancy::resolve(pool, UserId(user), workspace)
            .await
            .unwrap();
        let space = SpaceId::new();
        sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Chat', false)").bind(space).bind(workspace).execute(pool).await.unwrap();
        let team = team::create_default(pool, &tenant, space, Locale::Th)
            .await
            .unwrap();
        // Seed the first thread directly so route tests fail at the missing route, not in setup.
        let thread = ThreadId::new();
        sqlx::query("INSERT INTO threads (id, workspace_id, space_id, team_id, title) VALUES ($1,$2,$3,$4,'')").bind(thread).bind(workspace).bind(space).bind(team).execute(pool).await.unwrap();
        let app = app(pool, None);
        let cookie = cookie(&login(&app, &password).await);
        Self {
            app,
            cookie,
            tenant,
            space,
            team,
            thread,
        }
    }
    async fn call(&self, method: &str, suffix: &str, body: Option<Value>) -> Response {
        request(
            &self.app,
            method,
            &format!("/api/workspaces/{}/{suffix}", self.tenant.workspace_id()),
            body,
            Some(&self.cookie),
        )
        .await
    }
    async fn post(&self, body: Value) -> Response {
        self.call(
            "POST",
            &format!("threads/{}/messages", self.thread),
            Some(body),
        )
        .await
    }
    async fn page(&self, query: &str) -> Value {
        let response = self
            .call(
                "GET",
                &format!("threads/{}/messages{query}", self.thread),
                None,
            )
            .await;
        assert_eq!(response.status(), 200);
        body(response).await
    }
    async fn raw_post(&self, raw: String) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/workspaces/{}/threads/{}/messages",
                        self.tenant.workspace_id(),
                        self.thread
                    ))
                    .header("host", "localhost")
                    .header("origin", "http://localhost:3000")
                    .header("content-type", "application/json")
                    .header("cookie", &self.cookie)
                    .body(Body::from(raw))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    fn new_message(&self, text: &str) -> NewMessage {
        NewMessage {
            id: None,
            author: Author::User(self.tenant.user_id()),
            body: text.into(),
            client_msg_id: None,
            idempotency_key: None,
            synthetic: false,
            stopped: false,
            question: None,
            meta: None,
            job_id: None,
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn post_and_list_messages_in_order(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for text in ["  hello  ", "ช่วยสรุป 👩‍💻"] {
        assert_eq!(f.post(json!({"body": text})).await.status(), 201);
    }
    let page = f.page("").await;
    assert_eq!(page["event_seq"], 4);
    assert_eq!(page["messages"][0]["body"], "  hello  ");
    assert_eq!(page["messages"][1]["body"], "ช่วยสรุป 👩‍💻");
    assert_eq!(page["messages"][0]["synthetic"], false);
    assert_eq!(page["messages"][0]["stopped"], false);
    assert_eq!(
        page["messages"][0]["author"],
        json!({"kind":"user", "id":f.tenant.user_id()})
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn blank_body_is_422(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    assert_code(f.post(json!({"body":" \n\t "})).await, 422, "message_empty").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn nul_in_body_is_422_not_500(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    assert_code(f.post(json!({"body":"hi\0"})).await, 422, "message_invalid").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn body_limit_counts_utf8_bytes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (text, status) in [
        ("ก".repeat(10922), 201),
        ("ก".repeat(10923), 413),
        ("a".repeat(32768), 201),
        ("a".repeat(32769), 413),
        ("👩‍💻".repeat(3000), 413),
    ] {
        let response = f.post(json!({"body":text})).await;
        assert_eq!(response.status(), status);
        if status == 413 {
            assert_eq!(body(response).await, json!({"code":"message_too_large"}));
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn escaped_thai_body_at_the_cap_is_accepted(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    assert_eq!(
        f.raw_post(format!("{{\"body\":\"{}\"}}", "\\u0e01".repeat(10922)))
            .await
            .status(),
        201
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn retry_with_same_client_msg_id_returns_original_ids(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let post = json!({"body":"hello", "client_msg_id":"retry"});
    let first = f.post(post.clone()).await;
    assert_eq!(first.status(), 201);
    let first = body(first).await;
    let retry = f.post(post).await;
    assert_eq!(retry.status(), 200);
    assert_eq!(body(retry).await, first);
    assert_eq!(f.page("").await["messages"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn client_msg_id_blank_or_non_ascii_is_422(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (id, code) in [
        ("".into(), "client_msg_id_invalid"),
        ("ข้อความ".into(), "client_msg_id_invalid"),
        ("a".repeat(65), "client_msg_id_too_long"),
        ("ก".repeat(64), "client_msg_id_invalid"),
        ("ก".repeat(65), "client_msg_id_too_long"),
        ("a/b".into(), "client_msg_id_invalid"),
    ] {
        assert_code(
            f.post(json!({"body":"x", "client_msg_id":id})).await,
            422,
            code,
        )
        .await;
    }
    assert_eq!(
        f.post(json!({"body":"x", "client_msg_id":"a".repeat(64)}))
            .await
            .status(),
        201
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn client_msg_id_reuse_with_different_body_is_409_and_stores_nothing(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    assert_eq!(
        f.post(json!({"body":"first", "client_msg_id":"same"}))
            .await
            .status(),
        201
    );
    assert_code(
        f.post(json!({"body":"changed", "client_msg_id":"same"}))
            .await,
        409,
        "client_msg_id_conflict",
    )
    .await;
    let page = f.page("").await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    assert_eq!(page["messages"][0]["body"], "first");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn malformed_json_and_unknown_fields_return_code_json(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for raw in [
        "{\"body\":\"x\",\"author\":\"x\"}",
        "{\"body\":",
        "{\"body\":8}",
    ] {
        assert_code(f.raw_post(raw.into()).await, 400, "request_invalid").await;
    }
    assert_code(
        f.raw_post(" ".repeat(1_048_577)).await,
        413,
        "body_too_large",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn client_cannot_set_author_synthetic_or_job_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for field in [
        "author",
        "synthetic",
        "stopped",
        "job_id",
        "meta",
        "idempotency_key",
    ] {
        let mut post = json!({"body":"hello"});
        post[field] = json!(true);
        assert_code(f.post(post).await, 400, "request_invalid").await;
    }
    assert!(f.page("").await["messages"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thread_title_is_trimmed_bounded_and_nul_free(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (title, expected) in [("  แผนงาน  ", "แผนงาน"), ("👩‍💻", "👩‍💻"), ("", "")]
    {
        let response = f
            .call(
                "POST",
                "threads",
                Some(json!({"space_id":f.space,"team_id":f.team,"title":title})),
            )
            .await;
        assert_eq!(response.status(), 201);
        assert_eq!(body(response).await["title"], expected);
    }
    let response = f
        .call(
            "POST",
            "threads",
            Some(json!({"space_id":f.space,"team_id":f.team})),
        )
        .await;
    assert_eq!(body(response).await["title"], "");
    for title in [
        "ก".repeat(121),
        "hi\0".into(),
        "\t title".into(),
        "line\nfeed".into(),
    ] {
        assert_code(
            f.call(
                "POST",
                "threads",
                Some(json!({"space_id":f.space,"team_id":f.team,"title":title})),
            )
            .await,
            422,
            "thread_title_invalid",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thread_list_is_newest_first_and_get_returns_one(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let new = thread::create(&pool, &f.tenant, f.space, f.team, "new")
        .await
        .unwrap();
    let list = body(
        f.call("GET", &format!("spaces/{}/threads", f.space), None)
            .await,
    )
    .await;
    assert_eq!(list[0]["id"], json!(new.id));
    assert_eq!(list[1]["id"], json!(f.thread));
    let one = body(f.call("GET", &format!("threads/{}", new.id), None).await).await;
    assert_eq!(one, list[0]);
    let list = body(
        f.call("GET", &format!("spaces/{}/threads?limit=0", f.space), None)
            .await,
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    for query in ["?limit=0", "?limit=no", "?before=no"] {
        assert_code(
            f.call(
                "GET",
                &format!("threads/{}/messages{query}", f.thread),
                None,
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thread_create_rejects_team_from_another_space(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE memberships SET role='owner' WHERE workspace_id=$1")
        .bind(f.tenant.workspace_id())
        .execute(&pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&pool, f.tenant.user_id(), f.tenant.workspace_id())
        .await
        .unwrap();
    let other = tenancy::space::create(
        &pool,
        &tenant,
        "Other",
        SpacePolicy {
            allow_external: false,
        },
    )
    .await
    .unwrap();
    assert_code(
        f.call(
            "POST",
            "threads",
            Some(json!({"space_id":other,"team_id":f.team})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    let error = sqlx::query(
        "INSERT INTO threads (id,workspace_id,space_id,team_id,title) VALUES ($1,$2,$3,$4,'')",
    )
    .bind(ThreadId::new())
    .bind(tenant.workspace_id())
    .bind(other)
    .bind(f.team)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
}

async fn foreign_thread(pool: &PgPool) -> (SpaceId, TeamId, ThreadId) {
    let workspace = WorkspaceId::new();
    let space = SpaceId::new();
    let team = TeamId::new();
    let thread = ThreadId::new();
    sqlx::query("INSERT INTO workspaces (id,name) VALUES ($1,'Other')")
        .bind(workspace)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO spaces (id,workspace_id,name,allow_external) VALUES ($1,$2,'Other',false)",
    )
    .bind(space)
    .bind(workspace)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (id,workspace_id,space_id,name) VALUES ($1,$2,$3,'Other')")
        .bind(team)
        .bind(workspace)
        .bind(space)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads (id,workspace_id,space_id,team_id,title) VALUES ($1,$2,$3,$4,'')",
    )
    .bind(thread)
    .bind(workspace)
    .bind(space)
    .bind(team)
    .execute(pool)
    .await
    .unwrap();
    (space, team, thread)
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_thread_with_other_workspaces_space_is_404(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (space, team, _) = foreign_thread(&pool).await;
    assert_code(
        f.call(
            "POST",
            "threads",
            Some(json!({"space_id":space,"team_id":team})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    assert_code(
        f.call("GET", &format!("spaces/{space}/threads"), None)
            .await,
        404,
        "not_found",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn get_messages_of_other_workspace_thread_is_404(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, _, thread) = foreign_thread(&pool).await;
    for suffix in [
        format!("threads/{thread}"),
        format!("threads/{thread}/messages"),
    ] {
        assert_code(f.call("GET", &suffix, None).await, 404, "not_found").await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn post_to_other_workspace_thread_is_404(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, _, thread) = foreign_thread(&pool).await;
    assert_code(
        f.call(
            "POST",
            &format!("threads/{thread}/messages"),
            Some(json!({"body":"x"})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn chat_requires_authentication(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (method, suffix, post) in [
        ("GET", format!("threads/{}", f.thread), None),
        (
            "POST",
            format!("threads/{}/messages", f.thread),
            Some(json!({"body":"x"})),
        ),
    ] {
        assert_code(
            request(
                &f.app,
                method,
                &format!("/api/workspaces/{}/{suffix}", f.tenant.workspace_id()),
                post,
                None,
            )
            .await,
            401,
            "unauthenticated",
        )
        .await;
    }
}
