//! Real Docker scenarios, run by scripts/smoke.sh in CI. Each test owns its project and volumes.
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    time::Duration,
};
use uuid::Uuid;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn output(command: &mut Command) -> Output {
    command.output().expect("start command")
}
fn checked(command: &mut Command) -> String {
    let result = output(command);
    assert!(
        result.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
struct Stack {
    dir: PathBuf,
    project: String,
    files: String,
    password: String,
    image: String,
    https: bool,
    demo: bool,
    endpoint: String,
    client: reqwest::Client,
    cookie: String,
    database: String,
}
impl Stack {
    fn new(https: bool) -> Self {
        let project = format!("smoke-{}", Uuid::now_v7());
        let dir = std::env::temp_dir().join(&project);
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        checked(
            Command::new("sh")
                .arg("scripts/init-env.sh")
                .current_dir(ROOT)
                .env("KATWORK_ENV_FILE", dir.join(".env")),
        );
        fs::create_dir(dir.join("backups")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The private parent protects this directory; the cap-dropped container must write here.
            fs::set_permissions(dir.join("backups"), fs::Permissions::from_mode(0o777)).unwrap();
        }
        let ports = if https {
            "  katwork:\n    ports: !reset []\n  caddy:\n    ports: !override [\"127.0.0.1::443\", \"127.0.0.1::80\"]\n"
        } else {
            "  katwork:\n    ports: !override [\"127.0.0.1::3000\"]\n"
        };
        fs::write(dir.join("test.yml"), format!("services:\n{ports}  backup:\n    volumes:\n      - {}:/backups\n  restore:\n    image: postgres:17\n    networks: [backend]\n    environment:\n      POSTGRES_PASSWORD: ${{POSTGRES_PASSWORD}}\n    volumes:\n      - {}:/backups:ro\n", dir.join("backups").display(), dir.join("backups").display())).unwrap();
        let mut files = format!("{ROOT}/docker-compose.yml:{ROOT}/docker-compose.smoke.yml");
        if https {
            files.push_str(&format!(":{ROOT}/docker-compose.https.yml"));
        }
        files.push_str(&format!(":{}", dir.join("test.yml").display()));
        Self {
            dir,
            project,
            files,
            password: Uuid::now_v7().to_string(),
            image: std::env::var("KATWORK_IMAGE").unwrap_or_else(|_| "katwork-smoke:local".into()),
            https,
            demo: false,
            endpoint: String::new(),
            client: reqwest::Client::new(),
            cookie: String::new(),
            database: "katwork".into(),
        }
    }
    fn compose(&self) -> Command {
        let mut cmd = Command::new("docker");
        cmd.current_dir(ROOT)
            .args(["compose", "--project-name", &self.project, "--env-file"])
            .arg(self.dir.join(".env"))
            .env("COMPOSE_FILE", &self.files)
            .env("KATWORK_IMAGE", &self.image)
            .env("KATWORK_DEMO_PASSWORD", &self.password)
            .env("KATWORK_PUBLIC_URL", "http://localhost:3000")
            .env("KATWORK_HTTPS_HOST", "localhost")
            .env(
                "KATWORK_ALLOW_DEMO_ON_HTTPS",
                if self.https { "1" } else { "" },
            );
        for name in [
            "DATABASE_URL",
            "KATWORK_SECRET",
            "POSTGRES_PASSWORD",
            "COMPOSE_PROFILES",
        ] {
            cmd.env_remove(name);
        }
        cmd
    }
    fn run(&self, args: &[&str]) -> String {
        checked(self.compose().args(args))
    }
    fn sql(&self, sql: &str) -> String {
        self.run(&[
            "exec",
            "-T",
            "postgres",
            "psql",
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "katwork",
            "-d",
            &self.database,
            "-Atc",
            sql,
        ])
        .trim()
        .into()
    }
    fn public_url(&self) -> &str {
        if self.https {
            "https://localhost"
        } else if self.demo {
            "http://localhost:3001"
        } else {
            "http://localhost:3000"
        }
    }
    async fn start(&mut self) {
        self.run(&["up", "-d", "--no-build", "katwork"]);
        self.connect().await;
    }
    async fn connect(&mut self) {
        let (service, port) = if self.https {
            self.run(&["up", "-d", "--no-build", "caddy"]);
            ("caddy", "443")
        } else if self.demo {
            self.run(&["up", "-d", "--no-build", "demo-proxy"]);
            ("demo-proxy", "3000")
        } else {
            ("katwork", "3000")
        };
        let address = self.run(&["port", service, port]);
        let port = address.trim().rsplit(':').next().unwrap();
        self.endpoint = format!(
            "{}://localhost:{port}",
            if self.https { "https" } else { "http" }
        );
        let mut builder = reqwest::Client::builder()
            // The assigned transport port differs from the canonical Host; HTTP/2 rejects that pair.
            .http1_only()
            .no_proxy()
            .timeout(Duration::from_secs(180));
        if self.https {
            let root = self.dir.join("root.crt");
            poll(|| {
                output(
                    self.compose()
                        .args(["cp", "caddy:/data/caddy/pki/authorities/local/root.crt"])
                        .arg(&root),
                )
                .status
                .success()
            })
            .await;
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&fs::read(root).unwrap()).unwrap(),
            );
        }
        self.client = builder.build().unwrap();
        let mut last = String::new();
        let ready = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                match self.request("GET", "/healthz").send().await {
                    Ok(response) if response.status().is_success() => break,
                    Ok(response) => last = format!("health status {}", response.status()),
                    Err(error) => last = format!("{error:?}"),
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await;
        assert!(ready.is_ok(), "healthz becomes ready: {last}");
    }
    fn request(&self, method: &str, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method.parse().unwrap(), format!("{}{path}", self.endpoint))
            .header("Host", self.public_url().split_once("://").unwrap().1)
            .header("Origin", self.public_url())
            .header("Cookie", &self.cookie)
    }
    async fn json(&self, method: &str, path: &str, body: Value) -> Value {
        let response = self
            .request(method, path)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
    }
    async fn seed_login(&mut self) {
        self.run(&["exec", "-T", "katwork", "/app/katwork-server", "seed-demo"]);
        let response = self
            .request("POST", "/api/auth/login")
            .header("Content-Type", "application/json")
            .body(json!({"email":"demo@katwork.local","password":self.password}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert_eq!(cookie.contains("; Secure"), self.https);
        self.cookie = cookie.split(';').next().unwrap().into();
    }
    async fn thread(&self) -> String {
        let workspace = self.sql("SELECT id FROM workspaces WHERE name='เดโม'");
        let space = self.sql(&format!(
            "SELECT id FROM spaces WHERE workspace_id='{workspace}' LIMIT 1"
        ));
        let team = self.sql(&format!(
            "SELECT id FROM teams WHERE space_id='{space}' LIMIT 1"
        ));
        let path = format!("/api/workspaces/{workspace}/threads");
        let thread = self
            .json(
                "POST",
                &path,
                json!({"space_id":space,"team_id":team,"title":"Compose smoke"}),
            )
            .await;
        format!("{path}/{}", thread["id"].as_str().unwrap())
    }
    async fn post(&self, thread: &str) -> String {
        self.json(
            "POST",
            &format!("{thread}/messages"),
            json!({"body":"สวัสดี ช่วยแนะนำทีมหน่อย", "client_msg_id":Uuid::now_v7().to_string()}),
        )
        .await["job_id"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn answered(&self, thread: &str, job: &str) {
        let response = self
            .request("GET", &format!("{thread}/events?after=0"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mut stream = response.bytes_stream().eventsource();
        tokio::time::timeout(Duration::from_secs(150), async {
            while let Some(event) = stream.next().await {
                let event: Value = serde_json::from_str(&event.unwrap().data).unwrap();
                if event["name"] == "katwork.job_status"
                    && event["value"]["jobId"] == job
                    && !event["value"]["status"].is_null()
                {
                    assert_eq!(event["value"]["status"], "answered");
                    return;
                }
            }
            panic!("SSE closed before terminal status");
        })
        .await
        .expect("terminal job event");
        let messages = self
            .json("GET", &format!("{thread}/messages"), Value::Null)
            .await;
        let replies: Vec<_> = messages["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["job_id"] == job && m["author"]["kind"] == "kat")
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0]["synthetic"], true);
        assert!(replies[0]["body"].as_str().unwrap().contains("สวัสดี"));
    }
    fn effective_smoke(&self) {
        let id = self.run(&["ps", "-q", "katwork"]);
        let inspect: Value = serde_json::from_str(&checked(
            Command::new("docker").args(["inspect", id.trim()]),
        ))
        .unwrap();
        let env = inspect[0]["Config"]["Env"].as_array().unwrap();
        for setting in [
            "KATWORK_SMOKE_HOLD_SECS=20",
            "KATWORK_DEMO=1",
            "RUST_LOG=debug,rig=trace",
            "OPENAI_API_KEY=",
            "ANTHROPIC_API_KEY=",
            "KATWORK_LOCAL_BASE_URL=",
            "KATWORK_LOCAL_MODEL=",
        ] {
            assert!(
                env.iter().any(|v| v == setting),
                "missing smoke setting {setting}"
            );
        }
    }
}
impl Drop for Stack {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let logs = output(self.compose().args(["logs", "--no-color", "--tail", "40"]));
            eprintln!(
                "Compose diagnostics: {}",
                String::from_utf8_lossy(&logs.stdout)
            );
        }
        let _ = output(self.compose().args(["down", "-v", "--remove-orphans"]));
        let _ = fs::remove_dir_all(&self.dir);
    }
}
async fn poll(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(120), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("observed state before timeout");
}

#[test]
#[ignore = "Docker Compose CLI; run scripts/smoke.sh"]
fn first_run_and_profile_configuration() {
    checked(
        Command::new("sh")
            .arg("scripts/tests/init-env.sh")
            .current_dir(ROOT),
    );
    let stack = Stack::new(false);
    let missing = output(
        Command::new("docker")
            .current_dir(ROOT)
            .args(["compose", "--env-file", "/dev/null", "config", "-q"])
            .env("COMPOSE_FILE", "docker-compose.yml")
            .env("DATABASE_URL", "postgres://localhost/fixture")
            .env("POSTGRES_PASSWORD", Uuid::now_v7().to_string())
            .env_remove("KATWORK_SECRET"),
    );
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("KATWORK_SECRET") && error.contains("scripts/init-env.sh"));
    for variable in ["DATABASE_URL", "POSTGRES_PASSWORD", "KATWORK_SECRET"] {
        let missing = output(stack.compose().env(variable, "").args(["config", "-q"]));
        assert!(!missing.status.success());
        let error = String::from_utf8_lossy(&missing.stderr);
        assert!(error.contains(variable) && error.contains("scripts/init-env.sh"));
    }
    for overrides in [
        vec![],
        vec!["smoke"],
        vec!["https"],
        vec!["demo"],
        vec!["smoke", "https"],
        vec!["demo", "https"],
        vec!["smoke", "demo"],
        vec!["smoke", "demo", "https"],
    ] {
        let files = std::iter::once(format!("{ROOT}/docker-compose.yml"))
            .chain(
                overrides
                    .iter()
                    .map(|name| format!("{ROOT}/docker-compose.{name}.yml")),
            )
            .collect::<Vec<_>>()
            .join(":");
        checked(
            stack
                .compose()
                .env("COMPOSE_FILE", &files)
                .env("KATWORK_HTTPS_HOST", "smoke.example")
                .args(["config", "-q"]),
        );
        let config: Value = serde_json::from_str(&checked(
            stack
                .compose()
                .env("COMPOSE_FILE", files)
                .env("CARGO_FEATURES", "smoke-fixtures")
                .env("KATWORK_HTTPS_HOST", "smoke.example")
                .args(["config", "--format", "json"]),
        ))
        .unwrap();
        let app = &config["services"]["katwork"];
        assert_eq!(app["restart"], "unless-stopped");
        assert_eq!(config["services"]["postgres"]["restart"], "unless-stopped");
        assert_eq!(
            app["build"]["target"],
            if overrides.contains(&"smoke") {
                "smoke"
            } else {
                "release"
            }
        );
        if !overrides.contains(&"smoke") {
            assert!(app["build"]["args"].is_null());
        }
        assert_eq!(
            app["healthcheck"]["test"],
            json!(["CMD", "/app/katwork-server", "healthcheck"])
        );
        assert!(
            config["services"]["backup"]["environment"]["DATABASE_URL"]
                == config["services"]["migrate"]["environment"]["DATABASE_URL"],
            "backup and migrate must share DATABASE_URL"
        );
        assert_eq!(config["networks"]["backend"]["internal"], true);
        assert_eq!(
            config["services"]["postgres"]["networks"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert!(config["services"]["postgres"]["ports"].is_null());
        if overrides.contains(&"https") {
            assert_eq!(
                config["services"]["caddy"]["depends_on"]["katwork"]["condition"],
                "service_healthy"
            );
            assert_eq!(app["networks"]["edge"]["ipv4_address"], "172.31.250.3");
            assert_eq!(
                app["environment"]["KATWORK_TRUSTED_PROXIES"],
                "172.31.250.2"
            );
            assert_eq!(
                app["environment"]["KATWORK_PUBLIC_URL"],
                "https://smoke.example"
            );
            assert!(app["ports"].is_null() || app["ports"] == json!([]));
            for (source, target) in [("caddy_data", "/data"), ("caddy_config", "/config")] {
                assert!(
                    config["services"]["caddy"]["volumes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["source"] == source && v["target"] == target)
                );
            }
        }
        if overrides.contains(&"demo") {
            assert_eq!(
                config["services"]["demo-proxy"]["cap_add"],
                json!(["NET_BIND_SERVICE"])
            );
            assert!(app["ports"].is_null() || app["ports"] == json!([]));
            for network in app["networks"].as_object().unwrap().keys() {
                assert_eq!(config["networks"][network]["internal"], true);
            }
            assert!(
                config["services"]["demo-proxy"]["networks"]
                    .as_object()
                    .unwrap()
                    .contains_key("default")
            );
            assert_eq!(
                config["services"]["demo-proxy"]["ports"][0]["published"],
                "3001"
            );
            assert_eq!(app["environment"]["KATWORK_DEMO"], "1");
            assert_eq!(
                config["services"]["migrate"]["environment"]["KATWORK_DEMO"],
                "1"
            );
            for name in [
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "KATWORK_LOCAL_BASE_URL",
                "KATWORK_LOCAL_MODEL",
                "SMTP_HOST",
            ] {
                assert!(app["environment"][name] == "", "demo must clear {name}");
            }
        }
    }
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn thai_demo_stream_is_synthetic_and_dependency_logs_are_blocked() {
    let mut stack = Stack::new(false);
    stack.start().await;
    stack.effective_smoke();
    stack.seed_login().await;
    let html = stack.request("GET", "/").send().await.unwrap();
    assert_eq!(html.headers()["cache-control"], "no-store");
    let thread = stack.thread().await;
    let job = stack.post(&thread).await;
    stack.answered(&thread, &job).await;
    let logs = stack.run(&["logs", "--no-color", "katwork"]);
    assert!(logs.contains("smoke_fixture_started"));
    assert!(!logs.contains("smoke_dependency_probe"));
    assert!(
        !logs
            .lines()
            .any(|line| line.contains(" rig::") || line.contains(" rig_core::"))
    );
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn offline_demo_uses_synthetic_provider_with_host_keys_cleared() {
    let mut stack = Stack::new(false);
    stack.demo = true;
    let settings = stack.dir.join(".env");
    let mut env = fs::read_to_string(&settings).unwrap();
    env.push_str(&format!("\nOPENAI_API_KEY={}\nANTHROPIC_API_KEY={}\nKATWORK_LOCAL_BASE_URL=http://model.invalid/v1\nKATWORK_LOCAL_MODEL=fixture\nSMTP_HOST=mail.invalid\n", Uuid::now_v7(), Uuid::now_v7()));
    fs::write(settings, env).unwrap();

    let override_path = format!(":{}", stack.dir.join("test.yml").display());
    stack.files = stack.files.replace(
        &override_path,
        &format!(":{ROOT}/docker-compose.demo.yml{override_path}"),
    );
    let overrides = stack.dir.join("test.yml");
    let mut config = fs::read_to_string(&overrides).unwrap().replace("  katwork:\n    ports: !override [\"127.0.0.1::3000\"]", "  katwork:\n    ports: !reset []\n  demo-proxy:\n    ports: !override [\"127.0.0.1::3000\"]");
    config.push_str("  egress-target:\n    image: postgres:17\n    networks: [default]\n    environment:\n      POSTGRES_HOST_AUTH_METHOD: trust\n");
    fs::write(overrides, config).unwrap();
    stack.start().await;
    stack.seed_login().await;
    let id = stack.run(&["ps", "-q", "katwork"]);
    let inspect: Value = serde_json::from_str(&checked(
        Command::new("docker").args(["inspect", id.trim()]),
    ))
    .unwrap();
    let env = inspect[0]["Config"]["Env"].as_array().unwrap();
    for setting in [
        "OPENAI_API_KEY=",
        "ANTHROPIC_API_KEY=",
        "KATWORK_LOCAL_BASE_URL=",
        "KATWORK_LOCAL_MODEL=",
        "SMTP_HOST=",
    ] {
        assert!(
            env.iter().any(|value| value == setting),
            "demo must clear {setting}"
        );
    }
    stack.run(&["up", "-d", "egress-target"]);
    let target = stack.run(&["ps", "-q", "egress-target"]);
    let target_ip = checked(Command::new("docker").args([
        "inspect",
        "--format",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
        target.trim(),
    ]));
    let proxy = stack.run(&["ps", "-q", "demo-proxy"]);
    let can_connect = |container: &str| {
        output(Command::new("docker").args([
            "run",
            "--rm",
            "--network",
            &format!("container:{}", container.trim()),
            "postgres:17",
            "pg_isready",
            "-h",
            target_ip.trim(),
            "-t",
            "3",
        ]))
        .status
        .success()
    };
    // A reachable TCP listener outside backend rules out an unavailable internet test target.
    poll(|| can_connect(&proxy)).await;
    assert!(
        !can_connect(&id),
        "the app network namespace must have no outbound route"
    );
    let thread = stack.thread().await;
    let job = stack.post(&thread).await;
    stack.answered(&thread, &job).await;
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn restart_smoke_posts_one_reply() {
    let mut stack = Stack::new(false);
    stack.start().await;
    stack.seed_login().await;
    let thread = stack.thread().await;
    let job = stack.post(&thread).await;
    poll(|| stack.sql(&format!("SELECT count(*) FROM tasks t JOIN runs r ON r.task_id=t.id WHERE t.job_id='{job}' AND t.state='leased' AND r.state='running'")) == "1").await;
    stack.run(&["kill", "katwork"]);
    stack.run(&["up", "-d", "--no-build", "katwork"]);
    stack.connect().await;
    stack.effective_smoke();
    stack.answered(&thread, &job).await;
    assert_eq!(
        stack.sql(&format!(
            "SELECT count(*) FROM runs WHERE job_id='{job}' AND kind='recovery'"
        )),
        "1"
    );
    for name in ["katwork.decision", "katwork.job_status"] {
        let terminal = if name == "katwork.job_status" {
            " AND payload->'value'->>'status' IS NOT NULL"
        } else {
            ""
        };
        assert_eq!(stack.sql(&format!("SELECT count(*) FROM events WHERE payload->>'name'='{name}' AND payload->'value'->>'jobId'='{job}'{terminal}")), "1");
    }
    assert_eq!(
        stack.sql(&format!("SELECT status FROM jobs WHERE id='{job}'")),
        "answered"
    );
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn backup_archive_restores_seeded_data() {
    let mut stack = Stack::new(false);
    stack.start().await;
    assert_eq!(fs::read_dir(stack.dir.join("backups")).unwrap().count(), 0);
    stack.seed_login().await;
    let versions = stack.sql("SELECT version FROM _sqlx_migrations ORDER BY version");
    stack.run(&["stop", "katwork"]);
    stack.run(&[
        "exec",
        "-T",
        "postgres",
        "psql",
        "-U",
        "katwork",
        "-d",
        "postgres",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        "ALTER DATABASE katwork RENAME TO seeded",
    ]);
    stack.run(&[
        "exec", "-T", "postgres", "createdb", "-U", "katwork", "-O", "katwork", "katwork",
    ]);
    assert_eq!(
        stack.sql("SELECT to_regclass('public._sqlx_migrations') IS NULL"),
        "t"
    );
    let settings = stack.dir.join(".env");
    fs::write(
        &settings,
        fs::read_to_string(&settings)
            .unwrap()
            .replace("/katwork\n", "/seeded\n"),
    )
    .unwrap();
    stack.database = "seeded".into();
    stack.start().await;
    let archives: Vec<_> = fs::read_dir(stack.dir.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(archives.len(), 1);
    let archive = archives[0].to_str().unwrap();
    assert!(archive.ends_with(".dump"));
    stack.run(&["up", "-d", "restore"]);
    poll(|| {
        output(
            stack
                .compose()
                .args(["exec", "-T", "restore", "pg_isready", "-U", "postgres"]),
        )
        .status
        .success()
    })
    .await;
    stack.run(&[
        "exec",
        "-T",
        "restore",
        "createuser",
        "-U",
        "postgres",
        "katwork",
    ]);
    stack.run(&[
        "exec", "-T", "restore", "createdb", "-U", "postgres", "-O", "katwork", "katwork",
    ]);
    stack.run(&[
        "exec",
        "-T",
        "restore",
        "pg_restore",
        "--exit-on-error",
        "--no-owner",
        "-U",
        "katwork",
        "--dbname",
        "katwork",
        &format!("/backups/{archive}"),
    ]);
    let restored = |sql| {
        stack
            .run(&[
                "exec",
                "-T",
                "restore",
                "psql",
                "-X",
                "-v",
                "ON_ERROR_STOP=1",
                "-U",
                "katwork",
                "-d",
                "katwork",
                "-Atc",
                sql,
            ])
            .trim()
            .to_owned()
    };
    assert_eq!(
        restored("SELECT count(*) FROM users WHERE email='demo@katwork.local'"),
        "1"
    );
    assert_eq!(
        restored("SELECT version FROM _sqlx_migrations ORDER BY version"),
        versions
    );
    // Exercise the documented bundled-Postgres restore commands without replacing live data.
    stack.run(&["stop", "katwork"]);
    stack.run(&[
        "exec", "-T", "postgres", "createdb", "-U", "katwork", "-O", "katwork", "restored",
    ]);
    let restored_archive = stack.dir.join("restore.dump");
    stack.run(&[
        "cp",
        &format!("backup:/backups/{archive}"),
        restored_archive.to_str().unwrap(),
    ]);
    stack.run(&[
        "cp",
        restored_archive.to_str().unwrap(),
        "postgres:/tmp/restore.dump",
    ]);
    stack.run(&[
        "exec",
        "-T",
        "postgres",
        "pg_restore",
        "--exit-on-error",
        "--no-owner",
        "-U",
        "katwork",
        "-d",
        "restored",
        "/tmp/restore.dump",
    ]);
    stack.database = "restored".into();
    assert_eq!(
        stack.sql("SELECT count(*) FROM users WHERE email='demo@katwork.local'"),
        "1"
    );
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn backup_failure_blocks_migrate() {
    let mut stack = Stack::new(false);
    stack.start().await;
    stack.seed_login().await;
    stack.run(&["down"]);
    let failure = stack.dir.join("failure.yml");
    fs::write(
        &failure,
        "services:\n  backup:\n    entrypoint: [sh, -c, 'exit 1']\n",
    )
    .unwrap();
    let original = stack.files.clone();
    stack.files.push_str(&format!(":{}", failure.display()));
    assert!(
        !output(stack.compose().args(["up", "-d", "--no-build", "katwork"]))
            .status
            .success()
    );
    let id = stack.run(&["ps", "-aq", "backup"]);
    assert_eq!(
        checked(Command::new("docker").args([
            "inspect",
            "--format",
            "{{.State.ExitCode}}",
            id.trim()
        ]))
        .trim(),
        "1"
    );
    for service in ["migrate", "katwork"] {
        let id = stack.run(&["ps", "-aq", service]);
        if !id.trim().is_empty() {
            assert_eq!(
                checked(Command::new("docker").args([
                    "inspect",
                    "--format",
                    "{{.State.Status}}",
                    id.trim()
                ]))
                .trim(),
                "created"
            );
        }
    }
    stack.files = original;
    stack.start().await;
    assert_eq!(
        stack.sql("SELECT count(*) FROM users WHERE email='demo@katwork.local'"),
        "1"
    );
}

#[tokio::test]
#[ignore = "Docker daemon; run scripts/smoke.sh"]
async fn https_profile_survives_caddy_restart() {
    let mut stack = Stack::new(true);
    stack.start().await;
    let health = stack.request("GET", "/healthz").send().await.unwrap();
    assert_eq!(
        health.headers()["strict-transport-security"],
        "max-age=31536000"
    );
    let http = stack.run(&["port", "caddy", "80"]);
    let redirect = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("http://{}/healthz", http.trim()))
        .header("Host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(redirect.status(), 308);
    assert_eq!(redirect.headers()["location"], "https://localhost/healthz");
    stack.seed_login().await;
    let thread = stack.thread().await;
    let job = stack.post(&thread).await;
    // The deadline includes headers: a proxy buffering the whole response must fail too.
    let first = tokio::time::timeout(Duration::from_secs(1), async {
        let response = stack
            .request("GET", &format!("{thread}/events?after=0"))
            .header("Accept-Encoding", "gzip, br")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.headers().get("content-encoding").is_none());
        response
            .bytes_stream()
            .eventsource()
            .next()
            .await
            .unwrap()
            .unwrap()
    })
    .await
    .expect("unbuffered SSE");
    let event: Value = serde_json::from_str(&first.data).unwrap();
    assert!(event["type"].is_string());
    stack.answered(&thread, &job).await;
    let fingerprint = |stack: &Stack| {
        let port = stack.endpoint.rsplit(':').next().unwrap();
        let certificate = checked(
            Command::new("openssl")
                .args([
                    "s_client",
                    "-connect",
                    &format!("localhost:{port}"),
                    "-servername",
                    "localhost",
                    "-showcerts",
                ])
                .stdin(std::process::Stdio::null()),
        );
        use sha2::{Digest, Sha256};
        let start = certificate.find("-----BEGIN CERTIFICATE-----").unwrap();
        let end = certificate[start..]
            .find("-----END CERTIFICATE-----")
            .unwrap()
            + start;
        use base64::Engine;
        let encoded: String = certificate[start..end].lines().skip(1).collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        Sha256::digest(der).to_vec()
    };
    let before = fingerprint(&stack);
    let old_id = stack.run(&["ps", "-q", "caddy"]);
    stack.run(&["down"]);
    stack.start().await;
    assert_ne!(stack.run(&["ps", "-q", "caddy"]), old_id);
    assert_eq!(fingerprint(&stack), before);
}

#[tokio::test]
#[ignore = "Previous release image; run upgrade CI job"]
async fn upgrade_from_previous_release() {
    let Ok(previous) = std::env::var("KATWORK_PREVIOUS_IMAGE") else {
        eprintln!("SKIP upgrade_from_previous_release: no previous release image");
        return;
    };
    let mut stack = Stack::new(false);
    let current = std::mem::replace(&mut stack.image, previous);
    stack.start().await;
    stack.seed_login().await;
    let user = stack.sql("SELECT id FROM users WHERE email='demo@katwork.local'");
    stack.run(&["down"]);
    stack.image = current;
    stack.start().await;
    assert_eq!(
        stack.sql("SELECT id FROM users WHERE email='demo@katwork.local'"),
        user
    );
    assert!(
        fs::read_dir(stack.dir.join("backups")).unwrap().any(|e| e
            .unwrap()
            .path()
            .extension()
            .is_some_and(|x| x == "dump"))
    );
    stack.seed_login().await;
    let thread = stack.thread().await;
    let job = stack.post(&thread).await;
    stack.answered(&thread, &job).await;
}
