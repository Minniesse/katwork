#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_katwork-server"));
    command
        .env_clear()
        .env("DATABASE_URL", "postgres://localhost/katwork")
        .env("KATWORK_SECRET", uuid::Uuid::now_v7().to_string());
    command
}

#[test]
fn rejects_unknown_commands_and_extra_arguments() {
    for args in [vec!["unknown"], vec!["migrate", "unexpected"]] {
        let output = server().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("command_invalid"));
    }
}

#[test]
fn serve_rejects_missing_configuration() {
    let output = server()
        .arg("serve")
        .env_remove("KATWORK_SECRET")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("config_invalid"));
}

#[test]
fn serve_reports_database_cause_without_password() {
    let query_password = ["s3", "cret"].concat();
    for url in [
        "postgres://katwork:db-test-password@localhost/katwork?sslmode=db-test-password".to_owned(),
        "postgres://katwork:db%22pass%5Cword@localhost/katwork?sslmode=db%22pass%5Cword".to_owned(),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "p%40ss;word", "p%40ss;word"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "a%2Fb=c", "a%2Fb=c"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "päss%40rd", "invalid-secret"
        ),
        format!(
            "postgres://u:{}@localhost/db?sslmode={}",
            "pass\tword", "invalid-secret"
        ),
        format!("postgres://localhost/db?sslpassword={query_password}&sslmode={query_password}"),
        format!("postgres://localhost/db?PASSWORD={query_password}&sslmode={query_password}"),
    ] {
        let output = server()
            .arg("serve")
            .env("RUST_LOG", "trace")
            .env("DATABASE_URL", url)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(stderr.contains("database_unavailable"));
        assert!(stderr.contains("caused by:"), "{stderr}");
        assert!(stderr.contains("ssl_mode"), "{stderr}");
        assert!(stderr.contains("[redacted]"), "{stderr}");
        assert!(!stderr.contains("db-test-password"));
        assert!(!stderr.contains(r#"db\"pass\\word"#));
        for secret in [
            "p%40ss;word",
            "p@ss;word",
            "a%2Fb=c",
            "a/b=c",
            "päss",
            "pass\\tword",
            "invalid-secret",
            &query_password,
        ] {
            assert!(!stderr.contains(secret), "{stderr}");
        }
    }
}

#[test]
fn serve_reports_connection_refused_without_password() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let output = server()
        .arg("serve")
        .env(
            "DATABASE_URL",
            format!("postgres://u:{}@{address}/db", "p%40ss;word"),
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("database_unavailable"), "{stderr}");
    assert!(
        stderr.to_lowercase().contains("connection refused")
            || stderr.to_lowercase().contains("connection reset"),
        "{stderr}"
    );
    assert!(!stderr.contains("pool timed out"), "{stderr}");
    assert!(!stderr.contains("p%40ss;word"));
    assert!(!stderr.contains("p@ss;word"));
}

#[test]
fn serve_config_error_does_not_print_database_password() {
    let query_password = ["s3", "cret"].concat();
    let output = server()
        .arg("serve")
        .env_remove("KATWORK_SECRET")
        .env(
            "DATABASE_URL",
            format!(
                "postgres://u:{}@h/db?PASSWORD={query_password}",
                "p%40ss;word"
            ),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "config_invalid (KATWORK_SECRET)\n"
    );
}

#[test]
fn serve_short_query_values_finish_without_password() {
    let mut child = server()
        .arg("serve")
        .env("DATABASE_URL", "postgres://u:pw@h/db?sslmode=e&x=d")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(2) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("startup report did not finish within two seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("ssl_mode"), "{stderr}");
    assert!(!stderr.contains("pw"), "{stderr}");
    assert!(stderr.len() < 1024);
}

#[test]
fn migrate_rejects_missing_configuration() {
    let output = server()
        .arg("migrate")
        .env_remove("DATABASE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "config_invalid (DATABASE_URL)\n"
    );
}

#[sqlx::test(migrations = false)]
async fn migrate_command_applies_schema(pool: sqlx::PgPool) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut options = pool.connect_options().as_ref().clone();
    options = options.database(&database);
    let url = sqlx::ConnectOptions::to_url_lossy(&options).to_string();
    for _ in 0..2 {
        let output = server()
            .arg("migrate")
            .env("DATABASE_URL", &url)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

fn database_url(pool: &sqlx::PgPool) -> String {
    sqlx::ConnectOptions::to_url_lossy(pool.connect_options().as_ref()).to_string()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_creates_demo_user_and_workspace(pool: sqlx::PgPool) {
    let password = uuid::Uuid::now_v7().to_string();
    for _ in 0..2 {
        let output = server()
            .arg("seed-demo")
            .env("KATWORK_DEMO", "1")
            .env("DATABASE_URL", database_url(&pool))
            .env("KATWORK_DEMO_PASSWORD", &password)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&password));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&password));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id JOIN workspaces w ON w.id = m.workspace_id WHERE u.email = 'demo@katwork.local' AND m.role = 'owner'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
    let app = support::app(&pool, None);
    assert_eq!(demo_login(&app, &password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_reset_password_allows_login(pool: sqlx::PgPool) {
    let first = server()
        .arg("seed-demo")
        .env("KATWORK_DEMO", "1")
        .env("DATABASE_URL", database_url(&pool))
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_password = String::from_utf8(first.stdout).unwrap().trim().to_owned();
    assert!(first_password.len() >= 32);
    let second = server()
        .arg("seed-demo")
        .env("KATWORK_DEMO", "1")
        .env("DATABASE_URL", database_url(&pool))
        .output()
        .unwrap();
    assert!(second.status.success());
    assert!(second.stdout.is_empty());
    let app = support::app(&pool, None);
    let cookie = support::cookie(&demo_login(&app, &first_password).await);
    let password = uuid::Uuid::now_v7().to_string();
    let reset = server()
        .args(["seed-demo", "--reset-password"])
        .env("KATWORK_DEMO", "1")
        .env("DATABASE_URL", database_url(&pool))
        .env("KATWORK_DEMO_PASSWORD", &password)
        .output()
        .unwrap();
    assert!(reset.status.success());
    assert_eq!(String::from_utf8(reset.stdout).unwrap().trim(), password);
    assert_eq!(demo_login(&app, &password).await.status(), 200);
    support::assert_code(
        demo_login(&app, &first_password).await,
        401,
        "unauthenticated",
    )
    .await;
    support::assert_code(
        support::request(&app, "GET", "/api/auth/me", None, Some(&cookie)).await,
        401,
        "unauthenticated",
    )
    .await;
}

#[test]
fn seed_demo_requires_demo_mode_before_connecting() {
    let output = server().arg("seed-demo").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "config_invalid (KATWORK_DEMO)\n"
    );
}

async fn demo_login(app: &axum::Router, password: &str) -> axum::response::Response {
    support::request(
        app,
        "POST",
        "/api/auth/login",
        Some(serde_json::json!({
            "email": "demo@katwork.local", "password": password,
        })),
        None,
    )
    .await
}

fn operator(pool: &sqlx::PgPool, args: &[&str]) -> std::process::Output {
    server()
        .args(args)
        .env("DATABASE_URL", database_url(pool))
        .output()
        .unwrap()
}

/// The only line on a successful operator command's stdout.
fn printed_link(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let link = stdout.strip_suffix('\n').expect("link ends with a newline");
    assert!(!link.contains('\n'), "{stdout}");
    link.to_owned()
}

fn link_token(link: &str) -> String {
    let (_, token) = link.split_once("?token=").unwrap();
    assert_eq!(token.len(), 43, "{link}");
    token.to_owned()
}

fn assert_failed(output: &std::process::Output, stderr: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr), stderr);
}

async fn accept_invite(
    app: &axum::Router,
    token: &str,
    password: &str,
) -> axum::response::Response {
    support::request(
        app,
        "POST",
        "/api/auth/invite/accept",
        Some(serde_json::json!({"token": token, "new_password": password})),
        None,
    )
    .await
}

async fn confirm_reset(
    app: &axum::Router,
    token: &str,
    password: &str,
) -> axum::response::Response {
    support::request(
        app,
        "POST",
        "/api/auth/reset/confirm",
        Some(serde_json::json!({"token": token, "new_password": password})),
        None,
    )
    .await
}

async fn count(pool: &sqlx::PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_prints_a_single_use_set_password_link(pool: sqlx::PgPool) {
    let output = operator(
        &pool,
        &[
            "create-user",
            "--email",
            " Person@Example.test ",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "local",
        ],
    );
    let link = printed_link(&output);
    assert!(
        link.starts_with("http://localhost:3000/invite?token="),
        "{link}"
    );
    let token = link_token(&link);
    let app = support::app(&pool, None);
    let password = uuid::Uuid::now_v7().to_string();
    let accepted = accept_invite(&app, &token, &password).await;
    assert_eq!(accepted.status(), 204);
    assert_eq!(support::login(&app, &password).await.status(), 200);
    support::assert_code(
        accept_invite(&app, &token, &uuid::Uuid::now_v7().to_string()).await,
        400,
        "invite_token_invalid",
    )
    .await;
    assert_eq!(support::login(&app, &password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_creates_a_new_workspace_only_for_an_owner(pool: sqlx::PgPool) {
    for (email, workspace, policy, allow_external) in [
        ("local@example.test", "Local Co", "local", false),
        ("hosted@example.test", "Hosted Co", "hosted", true),
    ] {
        printed_link(&operator(
            &pool,
            &[
                "create-user",
                "--space-policy",
                policy,
                "--role",
                "owner",
                "--workspace",
                workspace,
                "--email",
                email,
            ],
        ));
        let (role, spaces): (String, Vec<(String, bool)>) = (
            sqlx::query_scalar("SELECT m.role FROM memberships m JOIN users u ON u.id = m.user_id JOIN workspaces w ON w.id = m.workspace_id WHERE u.email = $1 AND w.name = $2")
                .bind(email).bind(workspace).fetch_one(&pool).await.unwrap(),
            sqlx::query_as("SELECT s.name, s.allow_external FROM spaces s JOIN workspaces w ON w.id = s.workspace_id WHERE w.name = $1")
                .bind(workspace).fetch_all(&pool).await.unwrap(),
        );
        assert_eq!(role, "owner");
        assert_eq!(spaces, vec![("General".to_owned(), allow_external)]);
    }
    for policy in [None, Some("local")] {
        let mut args = vec![
            "create-user",
            "--email",
            "admin@example.test",
            "--workspace",
            "New Co",
            "--role",
            "admin",
        ];
        args.extend(
            policy
                .map(|policy| ["--space-policy", policy])
                .into_iter()
                .flatten(),
        );
        assert_failed(&operator(&pool, &args), "not_found\n");
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM users WHERE email = 'admin@example.test'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM workspaces WHERE name = 'New Co'"
        )
        .await,
        0
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_new_workspace_requires_space_policy(pool: sqlx::PgPool) {
    assert_failed(
        &operator(
            &pool,
            &[
                "create-user",
                "--email",
                "owner@example.test",
                "--workspace",
                "Acme",
                "--role",
                "owner",
            ],
        ),
        "config_invalid (--space-policy)\n",
    );
    for sql in [
        "SELECT count(*) FROM users",
        "SELECT count(*) FROM workspaces",
        "SELECT count(*) FROM spaces",
        "SELECT count(*) FROM memberships",
        "SELECT count(*) FROM password_resets",
    ] {
        assert_eq!(count(&pool, sql).await, 0, "{sql}");
    }
    printed_link(&operator(
        &pool,
        &[
            "create-user",
            "--email",
            "owner@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "hosted",
        ],
    ));
    // Joining an existing workspace needs no policy, and ignores one that is given.
    for (email, role, policy) in [
        ("member@example.test", "member", None),
        ("other-owner@example.test", "owner", Some("local")),
    ] {
        let mut args = vec![
            "create-user",
            "--email",
            email,
            "--workspace",
            "Acme",
            "--role",
            role,
        ];
        args.extend(
            policy
                .map(|policy| ["--space-policy", policy])
                .into_iter()
                .flatten(),
        );
        printed_link(&operator(&pool, &args));
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM workspaces").await, 1);
    let spaces: Vec<(String, bool)> = sqlx::query_as("SELECT name, allow_external FROM spaces")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(spaces, vec![("General".to_owned(), true)]);
    assert_eq!(count(&pool, "SELECT count(*) FROM memberships").await, 3);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_joins_an_existing_workspace_by_id_or_name(pool: sqlx::PgPool) {
    let workspace = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Acme')")
        .bind(workspace)
        .execute(&pool)
        .await
        .unwrap();
    let id = workspace.to_string();
    for (email, reference, role) in [
        ("admin@example.test", id.as_str(), "admin"),
        ("member@example.test", "Acme", "member"),
    ] {
        printed_link(&operator(
            &pool,
            &[
                "create-user",
                "--email",
                email,
                "--workspace",
                reference,
                "--role",
                role,
            ],
        ));
        let stored: String = sqlx::query_scalar("SELECT m.role FROM memberships m JOIN users u ON u.id = m.user_id WHERE u.email = $1 AND m.workspace_id = $2")
            .bind(email).bind(workspace).fetch_one(&pool).await.unwrap();
        assert_eq!(stored, role);
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM workspaces").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM spaces").await, 0);
    // An id names a workspace or nothing; it never creates one.
    let unknown = uuid::Uuid::now_v7().to_string();
    assert_failed(
        &operator(
            &pool,
            &[
                "create-user",
                "--email",
                "owner@example.test",
                "--workspace",
                &unknown,
                "--role",
                "owner",
                "--space-policy",
                "local",
            ],
        ),
        "not_found\n",
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM workspaces").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM users").await, 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_rejects_an_ambiguous_workspace_name(pool: sqlx::PgPool) {
    for _ in 0..2 {
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Twin')")
            .bind(uuid::Uuid::now_v7())
            .execute(&pool)
            .await
            .unwrap();
    }
    for role in ["member", "owner"] {
        assert_failed(
            &operator(
                &pool,
                &[
                    "create-user",
                    "--email",
                    "person@example.test",
                    "--workspace",
                    "Twin",
                    "--role",
                    role,
                    "--space-policy",
                    "local",
                ],
            ),
            "workspace_ambiguous\n",
        );
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM users").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM workspaces").await, 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_rejects_an_existing_email(pool: sqlx::PgPool) {
    let (id, password) = support::user(&pool).await;
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Acme')")
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    for email in ["person@example.test", " PERSON@Example.TEST "] {
        assert_failed(
            &operator(
                &pool,
                &[
                    "create-user",
                    "--email",
                    email,
                    "--workspace",
                    "Acme",
                    "--role",
                    "admin",
                ],
            ),
            "user_exists\n",
        );
    }
    let after: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, hash);
    for sql in [
        "SELECT count(*) FROM memberships",
        "SELECT count(*) FROM password_resets",
    ] {
        assert_eq!(count(&pool, sql).await, 0, "{sql}");
    }
    let app = support::app(&pool, None);
    assert_eq!(support::login(&app, &password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_link_lasts_72_hours_and_reset_link_30_minutes(pool: sqlx::PgPool) {
    let invite = link_token(&printed_link(&operator(
        &pool,
        &[
            "create-user",
            "--email",
            "person@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "local",
        ],
    )));
    let reset = link_token(&printed_link(&operator(
        &pool,
        &["reset-link", "--email", "person@example.test"],
    )));
    for (token, kind, lifetime) in [
        (&invite, "invite", "72 hours"),
        (&reset, "reset", "30 minutes"),
    ] {
        let within: bool = sqlx::query_scalar("SELECT expires_at BETWEEN clock_timestamp() + $3::interval - interval '1 minute' AND clock_timestamp() + $3::interval FROM password_resets WHERE token_sha256 = $1 AND kind = $2")
            .bind(format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(token.as_bytes())))
            .bind(kind)
            .bind(lifetime)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(within, "{kind}");
    }
    sqlx::query("UPDATE password_resets SET expires_at = clock_timestamp() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let app = support::app(&pool, None);
    let password = uuid::Uuid::now_v7().to_string();
    support::assert_code(
        accept_invite(&app, &invite, &password).await,
        400,
        "invite_token_invalid",
    )
    .await;
    support::assert_code(
        confirm_reset(&app, &reset, &password).await,
        400,
        "reset_token_invalid",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_link_prints_a_link_for_an_existing_account(pool: sqlx::PgPool) {
    let (_, password) = support::user(&pool).await;
    let app = support::app(&pool, None);
    let session = support::cookie(&support::login(&app, &password).await);
    let link = printed_link(&operator(
        &pool,
        &["reset-link", "--email", " Person@Example.test "],
    ));
    assert!(
        link.starts_with("http://localhost:3000/reset?token="),
        "{link}"
    );
    let new_password = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        confirm_reset(&app, &link_token(&link), &new_password)
            .await
            .status(),
        200
    );
    support::assert_code(
        support::request(&app, "GET", "/api/auth/me", None, Some(&session)).await,
        401,
        "unauthenticated",
    )
    .await;
    support::assert_code(
        support::login(&app, &password).await,
        401,
        "unauthenticated",
    )
    .await;
    assert_eq!(support::login(&app, &new_password).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reset_link_for_an_unknown_email_is_not_found(pool: sqlx::PgPool) {
    support::user(&pool).await;
    assert_failed(
        &operator(&pool, &["reset-link", "--email", "unknown@example.test"]),
        "not_found\n",
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM password_resets").await,
        0
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn operator_commands_need_no_smtp_or_demo_mode(pool: sqlx::PgPool) {
    // `server()` clears the environment: no SMTP_* variables, no KATWORK_DEMO, no public URL.
    let command = server();
    for name in ["SMTP_HOST", "KATWORK_DEMO", "KATWORK_PUBLIC_URL"] {
        assert!(
            !command
                .get_envs()
                .any(|(key, value)| key == name && value.is_some()),
            "{name}"
        );
    }
    printed_link(&operator(
        &pool,
        &[
            "create-user",
            "--email",
            "person@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "local",
        ],
    ));
    printed_link(&operator(
        &pool,
        &["reset-link", "--email", "person@example.test"],
    ));
    let output = server()
        .args(["reset-link", "--email", "person@example.test"])
        .env("DATABASE_URL", database_url(&pool))
        .env("KATWORK_DEMO", "0")
        .output()
        .unwrap();
    printed_link(&output);
}

#[test]
fn operator_commands_reject_bad_arguments() {
    let create = |rest: &[&'static str]| {
        let mut args = vec!["create-user"];
        args.extend_from_slice(rest);
        args
    };
    for args in [
        create(&["--workspace", "Acme", "--role", "owner"]),
        create(&["--email", "a@example.test", "--role", "owner"]),
        create(&["--email", "a@example.test", "--workspace", "Acme"]),
        create(&[
            "--email",
            "a@example.test",
            "--email",
            "b@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "local",
            "--space-policy",
            "local",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--name",
            "x",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "guest",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "Owner",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "cloud",
        ]),
        create(&["--email", "a@example.test", "--workspace", "Acme", "--role"]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "extra",
        ]),
        vec!["reset-link"],
        vec!["reset-link", "--email"],
        vec![
            "reset-link",
            "--email",
            "a@example.test",
            "--email",
            "b@example.test",
        ],
        vec!["reset-link", "--email", "a@example.test", "--role", "owner"],
        vec!["reset-link", "a@example.test"],
    ] {
        // The default DATABASE_URL points nowhere: arguments are checked before connecting.
        let output = server().args(&args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "command_invalid\n",
            "{args:?}"
        );
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn operator_links_never_reach_logs(pool: sqlx::PgPool) {
    for args in [
        &[
            "create-user",
            "--email",
            "person@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--space-policy",
            "local",
        ][..],
        &["reset-link", "--email", "person@example.test"][..],
    ] {
        let output = server()
            .args(args)
            .env("DATABASE_URL", database_url(&pool))
            .env("RUST_LOG", "trace")
            .output()
            .unwrap();
        let token = link_token(&printed_link(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(&token), "{stderr}");
        assert!(!stderr.contains("token="), "{stderr}");
    }
}

#[test]
fn serve_config_error_names_the_variable_not_its_value() {
    let output = server()
        .arg("serve")
        .env("KATWORK_PUBLIC_URL", "ftp://private-marker.example")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("config_invalid (KATWORK_PUBLIC_URL)"),
        "{stderr}"
    );
    assert!(!stderr.contains("private-marker"), "{stderr}");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_applies_changed_demo_password(pool: sqlx::PgPool) {
    let seed = |password: &str| {
        let output = server()
            .arg("seed-demo")
            .env("KATWORK_DEMO", "1")
            .env("DATABASE_URL", database_url(&pool))
            .env("KATWORK_DEMO_PASSWORD", password)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The operator chose the value, so the command never prints it.
        assert!(output.stdout.is_empty());
    };
    let first = uuid::Uuid::now_v7().to_string();
    seed(&first);
    let app = support::app(&pool, None);
    let session = support::cookie(&demo_login(&app, &first).await);
    seed(&first);
    assert_eq!(
        support::request(&app, "GET", "/api/auth/me", None, Some(&session))
            .await
            .status(),
        200
    );
    assert_eq!(demo_login(&app, &first).await.status(), 200);
    let second = uuid::Uuid::now_v7().to_string();
    seed(&second);
    assert_eq!(demo_login(&app, &second).await.status(), 200);
    support::assert_code(demo_login(&app, &first).await, 401, "unauthenticated").await;
    support::assert_code(
        support::request(&app, "GET", "/api/auth/me", None, Some(&session)).await,
        401,
        "unauthenticated",
    )
    .await;
}
