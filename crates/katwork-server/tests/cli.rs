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
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

#[test]
fn rejects_unknown_commands_and_extra_arguments() {
    for args in [
        vec!["unknown"],
        vec!["migrate", "unexpected"],
        vec!["healthcheck", "unexpected"],
    ] {
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
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id JOIN workspaces w ON w.id = m.workspace_id WHERE u.email = 'demo@katwork.local' AND m.role = 'owner' AND w.name = 'Demo'")
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
    assert_eq!(row_counts(&pool).await, [1, 2, 2, 2, 12, 12, 2, 12]);
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

fn seed_demo(pool: &sqlx::PgPool) {
    let output = server()
        .arg("seed-demo")
        .env("KATWORK_DEMO", "1")
        .env("DATABASE_URL", database_url(pool))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const ROW_COUNTS: [&str; 8] = [
    "SELECT count(*) FROM users",
    "SELECT count(*) FROM workspaces",
    "SELECT count(*) FROM memberships",
    "SELECT count(*) FROM spaces",
    "SELECT count(*) FROM kats",
    "SELECT count(*) FROM kat_spaces",
    "SELECT count(*) FROM teams",
    "SELECT count(*) FROM team_members",
];

async fn row_counts(pool: &sqlx::PgPool) -> Vec<i64> {
    let mut counts = Vec::new();
    for sql in ROW_COUNTS {
        counts.push(count(pool, sql).await);
    }
    counts
}

/// The Space and active members, in order, of the named workspace's default team.
async fn default_team(
    pool: &sqlx::PgPool,
    workspace: &str,
) -> (String, String, Vec<(String, bool, i32)>) {
    let (team, space): (String, String) = sqlx::query_as("SELECT t.name, s.name FROM teams t JOIN spaces s ON s.id = t.space_id JOIN workspaces w ON w.id = t.workspace_id WHERE w.name = $1 AND t.is_default")
        .bind(workspace).fetch_one(pool).await.unwrap();
    let members = sqlx::query_as("SELECT d.name, m.is_lead, d.max_concurrency FROM team_members m JOIN teams t ON t.id = m.team_id JOIN kats d ON d.id = m.kat_id JOIN workspaces w ON w.id = t.workspace_id WHERE w.name = $1 AND t.is_default AND m.active ORDER BY m.position")
        .bind(workspace).fetch_all(pool).await.unwrap();
    (team, space, members)
}

fn roster(names: [&str; 6]) -> Vec<(String, bool, i32)> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| ((*name).to_owned(), i == 0, if i == 0 { 3 } else { 1 }))
        .collect()
}

const EN_ROSTER: [&str; 6] = [
    "Lead",
    "Researcher",
    "Writer",
    "Analyst",
    "Scheduler",
    "Reviewer",
];
const TH_ROSTER: [&str; 6] = ["หัวหน้าทีม", "นักวิจัย", "นักเขียน", "นักวิเคราะห์", "เลขา", "ผู้ตรวจ"];

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_creates_a_thai_workspace_with_the_thai_roster(pool: sqlx::PgPool) {
    seed_demo(&pool);
    assert_eq!(row_counts(&pool).await, [1, 2, 2, 2, 12, 12, 2, 12]);
    let workspaces: Vec<(String, String)> = sqlx::query_as("SELECT w.name, m.role FROM memberships m JOIN users u ON u.id = m.user_id JOIN workspaces w ON w.id = m.workspace_id WHERE u.email = 'demo@katwork.local' ORDER BY w.name = 'Demo' DESC")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(
        workspaces,
        [
            ("Demo".to_owned(), "owner".to_owned()),
            ("เดโม".to_owned(), "owner".to_owned())
        ]
    );
    let spaces: Vec<(String, bool)> = sqlx::query_as("SELECT s.name, s.allow_external FROM spaces s JOIN workspaces w ON w.id = s.workspace_id WHERE w.name = 'เดโม'")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(spaces, [("พื้นที่ทดลอง".to_owned(), true)]);
    assert_eq!(
        default_team(&pool, "เดโม").await,
        (
            "ทีมเริ่มต้น".to_owned(),
            "พื้นที่ทดลอง".to_owned(),
            roster(TH_ROSTER)
        )
    );
    assert_eq!(
        default_team(&pool, "Demo").await,
        (
            "Default team".to_owned(),
            "Demo".to_owned(),
            roster(EN_ROSTER)
        )
    );
    let kats: Vec<(String, String)> =
        sqlx::query_as("SELECT name, instructions FROM kats ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    seed_demo(&pool);
    assert_eq!(row_counts(&pool).await, [1, 2, 2, 2, 12, 12, 2, 12]);
    let again: Vec<(String, String)> =
        sqlx::query_as("SELECT name, instructions FROM kats ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(again, kats);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn seed_demo_adds_only_the_thai_workspace_to_an_older_seed(pool: sqlx::PgPool) {
    seed_demo(&pool);
    let thai: uuid::Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'เดโม'")
        .fetch_one(&pool)
        .await
        .unwrap();
    // Roll the install back to a seed from before the Thai workspace and the new defaults.
    for sql in [
        "DELETE FROM team_members WHERE workspace_id = $1",
        "DELETE FROM teams WHERE workspace_id = $1",
        // A Kat and its default grant reference each other, so they go in one statement.
        "WITH grants AS (DELETE FROM kat_spaces WHERE workspace_id = $1) DELETE FROM kats WHERE workspace_id = $1",
        "DELETE FROM spaces WHERE workspace_id = $1",
        "DELETE FROM memberships WHERE workspace_id = $1",
        "DELETE FROM workspaces WHERE id = $1",
    ] {
        sqlx::query(sql).bind(thai).execute(&pool).await.unwrap();
    }
    sqlx::query("UPDATE kats SET instructions = 'Older instructions.', max_concurrency = 1")
        .execute(&pool)
        .await
        .unwrap();
    let english: Vec<(uuid::Uuid, String, i32)> =
        sqlx::query_as("SELECT id, instructions, max_concurrency FROM kats ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    seed_demo(&pool);
    assert_eq!(row_counts(&pool).await, [1, 2, 2, 2, 12, 12, 2, 12]);
    let kept: Vec<(uuid::Uuid, String, i32)> = sqlx::query_as("SELECT d.id, d.instructions, d.max_concurrency FROM kats d JOIN workspaces w ON w.id = d.workspace_id WHERE w.name = 'Demo' ORDER BY d.id")
        .fetch_all(&pool).await.unwrap();
    // Existing Kats keep their stored settings; only new Kats get the new defaults.
    assert_eq!(kept, english);
    assert_eq!(default_team(&pool, "เดโม").await.2, roster(TH_ROSTER));
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
async fn create_user_new_workspace_has_default_team(pool: sqlx::PgPool) {
    let link = printed_link(&operator(
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
    assert_eq!(
        default_team(&pool, "Acme").await,
        (
            "Default team".to_owned(),
            "General".to_owned(),
            roster(EN_ROSTER)
        )
    );
    let locale: String = sqlx::query_scalar("SELECT locale FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(locale, "en");
    // The new owner can start a thread with that team straight away.
    let app = support::app(&pool, None);
    let password = uuid::Uuid::now_v7().to_string();
    let accepted = accept_invite(&app, &link_token(&link), &password).await;
    assert_eq!(accepted.status(), 204);
    let cookie = support::cookie(&support::login(&app, &password).await);
    let (workspace, space): (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("SELECT workspace_id, id FROM spaces")
            .fetch_one(&pool)
            .await
            .unwrap();
    let teams = support::body(
        support::request(
            &app,
            "GET",
            &format!("/api/workspaces/{workspace}/spaces/{space}/teams"),
            None,
            Some(&cookie),
        )
        .await,
    )
    .await;
    assert_eq!(teams.as_array().unwrap().len(), 1, "{teams}");
    let thread = support::request(
        &app,
        "POST",
        &format!("/api/workspaces/{workspace}/threads"),
        Some(serde_json::json!({"space_id": space, "team_id": teams[0]["id"]})),
        Some(&cookie),
    )
    .await;
    assert_eq!(thread.status(), 201);
    // Joining an existing workspace, by name or id, adds no team.
    let id = workspace.to_string();
    for (email, reference, role) in [
        ("member@example.test", "Acme", "member"),
        ("other-owner@example.test", id.as_str(), "owner"),
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
                "--space-policy",
                "hosted",
                "--locale",
                "th",
            ],
        ));
    }
    assert_eq!(row_counts(&pool).await, [3, 1, 3, 1, 6, 6, 1, 6]);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_locale_sets_the_account_and_new_roster_language(pool: sqlx::PgPool) {
    for (email, workspace, locale) in [
        ("thai@example.test", "บริษัทไทย", Some("th")),
        ("english@example.test", "English Co", Some("en")),
        ("joiner@example.test", "บริษัทไทย", Some("th")),
        ("default@example.test", "บริษัทไทย", None),
    ] {
        let mut args = vec![
            "create-user",
            "--email",
            email,
            "--workspace",
            workspace,
            "--role",
            "owner",
            "--space-policy",
            "hosted",
        ];
        args.extend(
            locale
                .map(|locale| ["--locale", locale])
                .into_iter()
                .flatten(),
        );
        printed_link(&operator(&pool, &args));
    }
    let accounts: Vec<(String, String)> =
        sqlx::query_as("SELECT email, locale FROM users ORDER BY email")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        accounts,
        [
            ("default@example.test".to_owned(), "en".to_owned()),
            ("english@example.test".to_owned(), "en".to_owned()),
            ("joiner@example.test".to_owned(), "th".to_owned()),
            ("thai@example.test".to_owned(), "th".to_owned()),
        ]
    );
    assert_eq!(
        default_team(&pool, "บริษัทไทย").await,
        (
            "ทีมเริ่มต้น".to_owned(),
            "General".to_owned(),
            roster(TH_ROSTER)
        )
    );
    assert_eq!(default_team(&pool, "English Co").await.2, roster(EN_ROSTER));
    assert_eq!(count(&pool, "SELECT count(*) FROM teams").await, 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_roster_failure_rolls_back_the_account_and_workspace(pool: sqlx::PgPool) {
    sqlx::raw_sql("CREATE FUNCTION fail_kat() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected roster failure'; END $$; CREATE TRIGGER fail_roster BEFORE INSERT ON kats FOR EACH ROW EXECUTE FUNCTION fail_kat();")
        .execute(&pool).await.unwrap();
    let output = operator(
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
            "local",
        ],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).starts_with("database_error\n"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(row_counts(&pool).await, [0; 8]);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM password_resets").await,
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
    assert_eq!(
        command
            .get_envs()
            .find(|(key, _)| *key == "LLVM_PROFILE_FILE")
            .and_then(|(_, value)| value),
        std::env::var_os("LLVM_PROFILE_FILE").as_deref(),
    );
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
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--locale",
            "fr",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--locale",
            "TH",
        ]),
        create(&[
            "--email",
            "a@example.test",
            "--workspace",
            "Acme",
            "--role",
            "owner",
            "--locale",
            "th",
            "--locale",
            "th",
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
            format!("command_invalid\n{USAGE}"),
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

const USAGE: &str = "\
usage: katwork-server [serve]
       katwork-server healthcheck
       katwork-server migrate
       katwork-server seed-demo [--reset-password]
       katwork-server create-user --email <email> --workspace <name|id> --role <owner|admin|member> [--space-policy <local|hosted>] [--locale <en|th>]
       katwork-server reset-link --email <email>
";

#[test]
fn help_prints_usage() {
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["create-user", "--help"],
        vec!["reset-link", "--email", "a@example.test", "-h"],
    ] {
        let output = server().args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), USAGE, "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
    }
}

#[sqlx::test(migrations = false)]
async fn operator_commands_ask_for_migrate_on_an_unmigrated_database(pool: sqlx::PgPool) {
    let expected = "migration_required\n  run `katwork-server migrate` first, then try again\n";
    let create = [
        "create-user",
        "--email",
        "person@example.test",
        "--workspace",
        "Acme",
        "--role",
        "owner",
        "--space-policy",
        "local",
    ];
    let reset = ["reset-link", "--email", "person@example.test"];
    for args in [&create[..], &reset[..]] {
        assert_failed(&operator(&pool, args), expected);
    }
    // Part of the schema is no better: every migration this binary carries must be applied.
    let mut old = sqlx::migrate!("../katwork/migrations");
    old.migrations = old
        .iter()
        .filter(|migration| migration.version <= 5)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    old.run(&pool).await.unwrap();
    for args in [&create[..], &reset[..]] {
        assert_failed(&operator(&pool, args), expected);
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM users").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM workspaces").await, 0);
}

#[test]
fn serve_warns_when_a_lan_bind_has_no_public_url() {
    let warning = "KATWORK_BIND exposes KatWork beyond this host";
    for (bind, public_url, warns) in [
        (Some("0.0.0.0"), None, true),
        (Some("192.168.1.20"), Some("http://localhost:3000"), true),
        (Some("0.0.0.0"), Some("http://127.0.0.1:3000"), true),
        (Some("0.0.0.0"), Some("http://192.168.1.20:3000"), false),
        (Some("127.0.0.1"), None, false),
        (Some("::1"), None, false),
        (None, None, false),
    ] {
        let mut command = server();
        // An unparsable sslmode stops serve right after its startup checks, without a wait.
        command.arg("serve").env(
            "DATABASE_URL",
            "postgres://localhost/katwork?sslmode=unparsable",
        );
        if let Some(bind) = bind {
            command.env("KATWORK_BIND", bind);
        }
        if let Some(url) = public_url {
            command.env("KATWORK_PUBLIC_URL", url);
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert_eq!(
            stderr.contains(warning),
            warns,
            "{bind:?} {public_url:?}: {stderr}"
        );
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn a_closed_stdout_fails_cleanly_and_keeps_the_link_off_stderr(pool: sqlx::PgPool) {
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
        let mut child = server()
            .args(args)
            .env("DATABASE_URL", database_url(&pool))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Close the reading end before the command gets to print.
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert_eq!(
            stderr,
            "link_not_printed\n  the account change was saved, but its link could not be written to stdout\n  run `katwork-server reset-link --email <email>` for a new link\n"
        );
    }
    // The account was created before the print failed; reset-link recovers it.
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM users WHERE email = 'person@example.test'"
        )
        .await,
        1
    );
    printed_link(&operator(
        &pool,
        &["reset-link", "--email", "person@example.test"],
    ));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn create_user_rejects_a_malformed_email(pool: sqlx::PgPool) {
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'Acme')")
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    // 255 bytes: one over the limit.
    let long = format!("{}@example.test", "a".repeat(242));
    assert_eq!(long.len(), 255);
    for email in [
        "nobody",
        "a@b@example.test",
        "@example.test",
        "person@",
        "@",
        "   ",
        "",
        long.as_str(),
    ] {
        for args in [
            &[
                "create-user",
                "--email",
                email,
                "--workspace",
                "Acme",
                "--role",
                "member",
            ][..],
            &["reset-link", "--email", email][..],
        ] {
            assert_failed(&operator(&pool, args), "request_invalid\n");
        }
    }
    for sql in [
        "SELECT count(*) FROM users",
        "SELECT count(*) FROM memberships",
        "SELECT count(*) FROM password_resets",
    ] {
        assert_eq!(count(&pool, sql).await, 0, "{sql}");
    }
    // 254 bytes is still an address.
    let longest = format!("{}@example.test", "a".repeat(241));
    printed_link(&operator(
        &pool,
        &[
            "create-user",
            "--email",
            &longest,
            "--workspace",
            "Acme",
            "--role",
            "member",
        ],
    ));
}
