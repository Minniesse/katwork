#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;

use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use axum::{Router, response::Response};
use katwork::{
    auth::{
        mailer::{self, CapturingMailer, MailKind},
        session,
    },
    chat::{Author, NewMessage, message, thread},
    config::Config,
    ids::{SpaceId, UserId, WorkspaceId},
    runtime::Core,
    team::team,
    tenancy::{
        self, Role,
        members::{self, Invite, InviteDelivery},
    },
    types::Locale,
};
use katwork_server::app::{AppState, build_router};
use rand_core::OsRng;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{assert_code, body, request};
use tracing::instrument::WithSubscriber;
use uuid::Uuid;

struct Person {
    id: UserId,
    email: String,
    cookie: String,
}

/// An account with a cheap password hash and a live session.
async fn person(pool: &PgPool, email: &str, locale: &str) -> Person {
    let id = UserId::new();
    let password = Uuid::now_v7().to_string();
    let hash = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(1024, 1, 1, None).unwrap(),
    )
    .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
    .unwrap()
    .to_string();
    let display_name = email.split('@').next().unwrap();
    sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(email)
        .bind(hash)
        .bind(display_name)
        .bind(locale)
        .execute(pool)
        .await
        .unwrap();
    let token = session::create(pool, id).await.unwrap();
    Person {
        id,
        email: email.to_owned(),
        cookie: format!("kw_session={}", token.0),
    }
}

async fn workspace(pool: &PgPool, name: &str) -> WorkspaceId {
    let id = WorkspaceId::new();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn join(pool: &PgPool, workspace: WorkspaceId, user: &Person, role: &str) {
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace)
        .bind(user.id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
}

fn smtp_config() -> Config {
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        "KATWORK_PUBLIC_URL" => Some("http://localhost:3000".into()),
        "SMTP_HOST" => Some("smtp.example.test".into()),
        _ => None,
    })
    .unwrap()
}

/// A router whose mail goes to the returned capture.
fn app_with(pool: &PgPool, config: Config) -> (Router, Arc<CapturingMailer>) {
    let capture = Arc::new(CapturingMailer::default());
    let mut core = Core::new(pool.clone(), Arc::new(config));
    core.mailer = capture.clone();
    (
        build_router(AppState {
            core: Arc::new(core),
        }),
        capture,
    )
}

fn plain_app(pool: &PgPool) -> Router {
    support::app(pool, None)
}

async fn call(
    app: &Router,
    who: &Person,
    method: &str,
    workspace: WorkspaceId,
    suffix: &str,
    body: Option<Value>,
) -> Response {
    request(
        app,
        method,
        &format!("/api/workspaces/{workspace}/members{suffix}"),
        body,
        Some(&who.cookie),
    )
    .await
}

fn invite(email: &str, role: &str, delivery: &str) -> Value {
    json!({"email": email, "role": role, "delivery": delivery})
}

async fn wait_for_mail(capture: &CapturingMailer, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while capture.sent().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the invite mail was never sent");
}

async fn users_with_email(pool: &PgPool, email: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn role_of(pool: &PgPool, workspace: WorkspaceId, user: UserId) -> Option<String> {
    sqlx::query_scalar("SELECT role FROM memberships WHERE workspace_id = $1 AND user_id = $2")
        .bind(workspace)
        .bind(user)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cannot_list_invite_or_change_roles(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let member = person(&pool, "member@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &member, "member").await;

    assert_code(
        call(&app, &member, "GET", ws, "", None).await,
        403,
        "forbidden",
    )
    .await;
    for body in [
        invite("new@example.test", "member", "link"),
        json!({"email": 5}),
    ] {
        assert_code(
            call(&app, &member, "POST", ws, "", Some(body)).await,
            403,
            "forbidden",
        )
        .await;
    }
    let target = format!("/{}/role", owner.id);
    for (suffix, body) in [
        (target.as_str(), json!({"role": "member"})),
        (target.as_str(), json!({"role": "emperor"})),
        ("/not-a-uuid/role", json!({"role": "member"})),
    ] {
        assert_code(
            call(&app, &member, "PUT", ws, suffix, Some(body)).await,
            403,
            "forbidden",
        )
        .await;
    }
    for suffix in [format!("/{}", owner.id), "/not-a-uuid".to_owned()] {
        assert_code(
            call(&app, &member, "DELETE", ws, &suffix, None).await,
            403,
            "forbidden",
        )
        .await;
    }
    assert_eq!(users_with_email(&pool, "new@example.test").await, 0);
    assert_eq!(role_of(&pool, ws, owner.id).await.as_deref(), Some("owner"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_new_email_by_link_returns_a_single_use_link(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;

    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("  New.Person@Example.test ", "admin", "link")),
    )
    .await;
    assert_eq!(response.status(), 201);
    let result = body(response).await;
    assert_eq!(result["created"], true);
    let link = result["link"].as_str().unwrap();
    assert!(link.starts_with("http://localhost:3000/invite?token="));
    let token = link.split_once("token=").unwrap().1;
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind("new.person@example.test")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["user_id"], json!(user_id));

    let password = Uuid::now_v7().to_string();
    let accept = || {
        request(
            &app,
            "POST",
            "/api/auth/invite/accept",
            Some(json!({"token": token, "new_password": password})),
            None,
        )
    };
    assert_eq!(accept().await.status(), 204);
    assert_code(accept().await, 400, "invite_token_invalid").await;

    let login = request(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "new.person@example.test", "password": password})),
        None,
    )
    .await;
    assert_eq!(login.status(), 200);
    let cookie = support::cookie(&login);
    let listed = body(request(&app, "GET", "/api/workspaces", None, Some(&cookie)).await).await;
    assert_eq!(listed[0]["id"], json!(ws));
    assert_eq!(listed[0]["role"], "admin");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_by_email_sends_a_bilingual_invite_mail(pool: PgPool) {
    let (app, capture) = app_with(&pool, smtp_config());
    let ws = workspace(&pool, "สำนักงานใหญ่").await;
    let owner = person(&pool, "owner@example.test", "th").await;
    join(&pool, ws, &owner, "owner").await;

    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("invitee@example.test", "member", "email")),
    )
    .await;
    assert_eq!(response.status(), 201);
    let result = body(response).await;
    assert_eq!(result["created"], true);
    assert_eq!(result["link"], Value::Null);

    wait_for_mail(&capture, 1).await;
    let sent = capture.sent();
    assert_eq!(sent.len(), 1);
    let mail = &sent[0];
    assert!(matches!(mail.kind, MailKind::Invite));
    assert_eq!(mail.to, "invitee@example.test");
    // The new account takes the inviter's locale, so the Thai half comes first.
    assert_eq!(
        mail.subject,
        "คำเชิญเข้าร่วม KatWork / You're invited to KatWork"
    );
    let lines: Vec<&str> = mail.body.lines().collect();
    assert_eq!(
        lines[0],
        "คุณได้รับเชิญเข้าร่วม สำนักงานใหญ่ บน KatWork ใช้ลิงก์นี้ภายใน 72 ชั่วโมงเพื่อตั้งรหัสผ่าน"
    );
    assert_eq!(
        lines[2],
        "You have been invited to สำนักงานใหญ่ on KatWork. Use this link within 72 hours to set your password."
    );
    let link = lines.last().unwrap();
    assert!(link.starts_with("http://localhost:3000/invite?token="));
    assert_eq!(mail.body.matches("token=").count(), 1);
    let locale: String = sqlx::query_scalar("SELECT locale FROM users WHERE email = $1")
        .bind("invitee@example.test")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(locale, "th");

    let english = mailer::invite_mail("a@example.test".into(), Locale::En, link, "Office");
    assert_eq!(
        english.subject,
        "You're invited to KatWork / คำเชิญเข้าร่วม KatWork"
    );
    assert!(
        english
            .body
            .starts_with("You have been invited to Office on KatWork.")
    );
    assert_eq!(english.body.lines().last(), Some(*link));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_by_email_without_smtp_is_422_and_writes_nothing(pool: PgPool) {
    let demo = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        "KATWORK_DEMO" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let existing = person(&pool, "existing@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    // The demo console mailer prints only the demo account's mail, so it is not SMTP.
    for app in [plain_app(&pool), app_with(&pool, demo).0] {
        for email in ["fresh@example.test", existing.email.as_str()] {
            assert_code(
                call(
                    &app,
                    &owner,
                    "POST",
                    ws,
                    "",
                    Some(invite(email, "member", "email")),
                )
                .await,
                422,
                "smtp_unconfigured",
            )
            .await;
        }
    }
    assert_eq!(users_with_email(&pool, "fresh@example.test").await, 0);
    assert_eq!(role_of(&pool, ws, existing.id).await, None);
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tokens, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_of_an_existing_account_adds_membership_without_a_link(pool: PgPool) {
    let (app, capture) = app_with(&pool, smtp_config());
    let ws = workspace(&pool, "Office").await;
    let elsewhere = workspace(&pool, "Elsewhere").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let existing = person(&pool, "existing@example.test", "en").await;
    let second = person(&pool, "second@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, elsewhere, &existing, "owner").await;
    join(&pool, elsewhere, &second, "member").await;
    let hash_of = |user: UserId| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT password_hash FROM users WHERE id = $1")
                .bind(user)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let before = (hash_of(existing.id).await, hash_of(second.id).await);

    for (who, delivery) in [(&existing, "link"), (&second, "email")] {
        let response = call(
            &app,
            &owner,
            "POST",
            ws,
            "",
            Some(invite(&who.email.to_uppercase(), "member", delivery)),
        )
        .await;
        assert_eq!(response.status(), 201);
        assert_eq!(
            body(response).await,
            json!({"user_id": who.id, "created": false, "link": null})
        );
        assert_eq!(role_of(&pool, ws, who.id).await.as_deref(), Some("member"));
    }
    assert_eq!(
        (hash_of(existing.id).await, hash_of(second.id).await),
        before
    );
    for who in [&existing, &second] {
        assert!(
            session::authenticate(&pool, who.cookie.trim_start_matches("kw_session="))
                .await
                .is_ok()
        );
    }
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tokens, 0);

    // A mail for a new account arrives after any the existing ones could have caused.
    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("fresh@example.test", "member", "email")),
    )
    .await;
    assert_eq!(response.status(), 201);
    wait_for_mail(&capture, 1).await;
    let recipients: Vec<String> = capture.sent().into_iter().map(|mail| mail.to).collect();
    assert_eq!(recipients, ["fresh@example.test"]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_of_an_existing_member_is_409(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let admin = person(&pool, "admin@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &admin, "admin").await;

    for email in [&admin.email, &owner.email] {
        assert_code(
            call(
                &app,
                &owner,
                "POST",
                ws,
                "",
                Some(invite(email, "member", "link")),
            )
            .await,
            409,
            "member_exists",
        )
        .await;
    }
    assert_eq!(role_of(&pool, ws, admin.id).await.as_deref(), Some("admin"));
    assert_eq!(role_of(&pool, ws, owner.id).await.as_deref(), Some("owner"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn only_an_owner_can_invite_or_promote_an_owner(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let admin = person(&pool, "admin@example.test", "en").await;
    let member = person(&pool, "member@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &admin, "admin").await;
    join(&pool, ws, &member, "member").await;

    assert_code(
        call(
            &app,
            &admin,
            "POST",
            ws,
            "",
            Some(invite("boss@example.test", "owner", "link")),
        )
        .await,
        403,
        "forbidden",
    )
    .await;
    assert_eq!(users_with_email(&pool, "boss@example.test").await, 0);
    let promote = format!("/{}/role", member.id);
    let demote_owner = format!("/{}/role", owner.id);
    for (suffix, role) in [(&promote, "owner"), (&demote_owner, "member")] {
        assert_code(
            call(&app, &admin, "PUT", ws, suffix, Some(json!({"role": role}))).await,
            403,
            "forbidden",
        )
        .await;
    }
    assert_code(
        call(&app, &admin, "DELETE", ws, &format!("/{}", owner.id), None).await,
        403,
        "forbidden",
    )
    .await;
    assert_eq!(
        role_of(&pool, ws, member.id).await.as_deref(),
        Some("member")
    );
    assert_eq!(role_of(&pool, ws, owner.id).await.as_deref(), Some("owner"));

    // An admin may still invite and promote up to admin.
    let response = call(
        &app,
        &admin,
        "POST",
        ws,
        "",
        Some(invite("helper@example.test", "admin", "link")),
    )
    .await;
    assert_eq!(response.status(), 201);
    let response = call(
        &app,
        &admin,
        "PUT",
        ws,
        &promote,
        Some(json!({"role": "admin"})),
    )
    .await;
    assert_eq!(response.status(), 204);

    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("boss@example.test", "owner", "link")),
    )
    .await;
    assert_eq!(response.status(), 201);
    let response = call(
        &app,
        &owner,
        "PUT",
        ws,
        &promote,
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(response.status(), 204);
    assert_eq!(
        role_of(&pool, ws, member.id).await.as_deref(),
        Some("owner")
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn last_owner_cannot_be_demoted(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let other = person(&pool, "other@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &other, "admin").await;
    let own_role = format!("/{}/role", owner.id);

    assert_code(
        call(
            &app,
            &owner,
            "PUT",
            ws,
            &own_role,
            Some(json!({"role": "admin"})),
        )
        .await,
        422,
        "last_owner_required",
    )
    .await;
    assert_code(
        call(&app, &owner, "DELETE", ws, &format!("/{}", owner.id), None).await,
        422,
        "last_owner_required",
    )
    .await;
    assert_eq!(role_of(&pool, ws, owner.id).await.as_deref(), Some("owner"));

    let response = call(
        &app,
        &owner,
        "PUT",
        ws,
        &format!("/{}/role", other.id),
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(response.status(), 204);
    let response = call(
        &app,
        &owner,
        "PUT",
        ws,
        &own_role,
        Some(json!({"role": "admin"})),
    )
    .await;
    assert_eq!(response.status(), 204);
    assert_eq!(role_of(&pool, ws, owner.id).await.as_deref(), Some("admin"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn role_change_applies_on_the_next_request(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let admin = person(&pool, "admin@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &admin, "admin").await;
    let target = format!("/{}/role", admin.id);

    let listed = call(&app, &admin, "GET", ws, "", None).await;
    assert_eq!(listed.status(), 200);
    let members = body(listed).await;
    // Ordered by email.
    assert_eq!(
        members,
        json!([
            {"user_id": admin.id, "email": "admin@example.test", "display_name": "admin", "role": "admin"},
            {"user_id": owner.id, "email": "owner@example.test", "display_name": "owner", "role": "owner"},
        ])
    );

    let response = call(
        &app,
        &owner,
        "PUT",
        ws,
        &target,
        Some(json!({"role": "member"})),
    )
    .await;
    assert_eq!(response.status(), 204);
    assert_code(
        call(&app, &admin, "GET", ws, "", None).await,
        403,
        "forbidden",
    )
    .await;

    let response = call(
        &app,
        &owner,
        "PUT",
        ws,
        &target,
        Some(json!({"role": "admin"})),
    )
    .await;
    assert_eq!(response.status(), 204);
    assert_eq!(call(&app, &admin, "GET", ws, "", None).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_invites_for_one_email_create_one_account(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let other = workspace(&pool, "Other").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let other_owner = person(&pool, "other.owner@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, other, &other_owner, "owner").await;

    // One workspace: the workspace lock serializes them, so the second finds a member.
    let body_for = || Some(invite("twice@example.test", "member", "link"));
    let (first, second) = tokio::join!(
        call(&app, &owner, "POST", ws, "", body_for()),
        call(&app, &owner, "POST", ws, "", body_for()),
    );
    let mut statuses = [first.status().as_u16(), second.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [201, 409]);
    assert_eq!(users_with_email(&pool, "twice@example.test").await, 1);

    // Two workspaces: the account's unique email makes one invite create it and the other
    // only add a membership, with no link.
    let body_for = || Some(invite("shared@example.test", "member", "link"));
    let (first, second) = tokio::join!(
        call(&app, &owner, "POST", ws, "", body_for()),
        call(&app, &other_owner, "POST", other, "", body_for()),
    );
    let (first, second) = (body(first).await, body(second).await);
    assert_eq!(users_with_email(&pool, "shared@example.test").await, 1);
    assert_eq!(first["user_id"], second["user_id"]);
    let mut created = [first["created"].clone(), second["created"].clone()];
    created.sort_by_key(|value| value.as_bool());
    assert_eq!(created, [json!(false), json!(true)]);
    let links = [&first, &second]
        .iter()
        .filter(|result| result["link"].is_string())
        .count();
    assert_eq!(links, 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn members_routes_hide_other_workspaces(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let other = workspace(&pool, "Other").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let outsider = person(&pool, "outsider@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, other, &outsider, "owner").await;

    // A member of another workspace sees nothing of this one.
    for (method, suffix, body) in [
        ("GET", String::new(), None),
        (
            "POST",
            String::new(),
            Some(invite("new@example.test", "member", "link")),
        ),
        (
            "PUT",
            format!("/{}/role", owner.id),
            Some(json!({"role": "member"})),
        ),
        ("DELETE", format!("/{}", owner.id), None),
    ] {
        assert_code(
            call(&app, &outsider, method, ws, &suffix, body).await,
            404,
            "not_found",
        )
        .await;
    }
    // A user id from another workspace is not a member here.
    assert_code(
        call(
            &app,
            &owner,
            "PUT",
            ws,
            &format!("/{}/role", outsider.id),
            Some(json!({"role": "admin"})),
        )
        .await,
        404,
        "not_found",
    )
    .await;
    assert_code(
        call(
            &app,
            &owner,
            "DELETE",
            ws,
            &format!("/{}", outsider.id),
            None,
        )
        .await,
        404,
        "not_found",
    )
    .await;
    assert_eq!(
        role_of(&pool, other, outsider.id).await.as_deref(),
        Some("owner")
    );
    assert_eq!(users_with_email(&pool, "new@example.test").await, 0);
}

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn subscriber(logs: &Logs) -> impl tracing::Subscriber + Send + Sync + 'static {
    let logs = logs.clone();
    tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || logs.clone())
        .finish()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn invite_tokens_never_reach_logs(pool: PgPool) {
    let (app, capture) = app_with(&pool, smtp_config());
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    let logs = Logs::default();

    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("linked@example.test", "member", "link")),
    )
    .with_subscriber(subscriber(&logs))
    .await;
    let link = body(response).await["link"].as_str().unwrap().to_owned();
    let response = call(
        &app,
        &owner,
        "POST",
        ws,
        "",
        Some(invite("mailed@example.test", "member", "email")),
    )
    .with_subscriber(subscriber(&logs))
    .await;
    assert_eq!(response.status(), 201);
    wait_for_mail(&capture, 1).await;
    let mailed = capture.sent()[0].body.lines().last().unwrap().to_owned();
    let password = Uuid::now_v7().to_string();
    let response = request(
        &app,
        "POST",
        "/api/auth/invite/accept",
        Some(json!({"token": link.split_once("token=").unwrap().1, "new_password": password})),
        None,
    )
    .with_subscriber(subscriber(&logs))
    .await;
    assert_eq!(response.status(), 204);

    // The library result never prints its link either.
    let tenant = tenancy::resolve(&pool, owner.id, ws).await.unwrap();
    let config = smtp_config();
    let result = members::invite(
        &pool,
        &config,
        capture.clone(),
        &tenant,
        Invite {
            email: "debug@example.test".into(),
            role: Role::Member,
            delivery: InviteDelivery::Link,
        },
    )
    .with_subscriber(subscriber(&logs))
    .await
    .unwrap();
    let debug_link = result.link.clone().unwrap();
    assert!(!format!("{result:?}").contains(&debug_link));

    for link in [&link, &mailed, &debug_link] {
        let token = link.split_once("token=").unwrap().1;
        assert!(!logs.text().contains(token), "a token reached the logs");
    }
    assert!(!logs.text().contains(&password));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn removing_a_member_keeps_their_message_history(pool: PgPool) {
    let app = plain_app(&pool);
    let ws = workspace(&pool, "Office").await;
    let owner = person(&pool, "owner@example.test", "en").await;
    let leaver = person(&pool, "leaver@example.test", "en").await;
    join(&pool, ws, &owner, "owner").await;
    join(&pool, ws, &leaver, "member").await;
    let space = SpaceId::new();
    sqlx::query(
        "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'General', false)",
    )
    .bind(space)
    .bind(ws)
    .execute(&pool)
    .await
    .unwrap();
    let owner_tenant = tenancy::resolve(&pool, owner.id, ws).await.unwrap();
    let team_id = team::create_default(&pool, &owner_tenant, space, Locale::En)
        .await
        .unwrap();
    let chat = thread::create(&pool, &owner_tenant, space, team_id, "Plans")
        .await
        .unwrap();
    let leaver_tenant = tenancy::resolve(&pool, leaver.id, ws).await.unwrap();
    message::append(
        &mut pool.acquire().await.unwrap(),
        &leaver_tenant,
        chat.id,
        NewMessage {
            id: None,
            author: Author::User(leaver.id),
            body: "My last note".into(),
            client_msg_id: None,
            idempotency_key: None,
            synthetic: false,
            stopped: false,
            question: None,
            meta: None,
            job_id: None,
        },
    )
    .await
    .unwrap();

    let response = call(&app, &owner, "DELETE", ws, &format!("/{}", leaver.id), None).await;
    assert_eq!(response.status(), 204);
    assert_eq!(role_of(&pool, ws, leaver.id).await, None);
    let listed = body(call(&app, &owner, "GET", ws, "", None).await).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_code(
        request(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/threads/{}/messages", chat.id),
            None,
            Some(&leaver.cookie),
        )
        .await,
        404,
        "not_found",
    )
    .await;

    let page = body(
        request(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/threads/{}/messages", chat.id),
            None,
            Some(&owner.cookie),
        )
        .await,
    )
    .await;
    let kept = &page["messages"][0];
    assert_eq!(kept["body"], "My last note");
    assert_eq!(kept["author"], json!({"kind": "user", "id": leaver.id}));
    assert_eq!(kept["author_name"], "leaver");
    assert_code(
        call(&app, &owner, "DELETE", ws, &format!("/{}", leaver.id), None).await,
        404,
        "not_found",
    )
    .await;
}
