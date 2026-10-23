use super::*;
use std::time::Duration;

pub(super) async fn lock_workspace(
    pool: &PgPool,
    workspace: Uuid,
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM workspaces WHERE id=$1 FOR NO KEY UPDATE")
        .bind(workspace)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx
}

pub(super) async fn wait_for_blocked_writers(pool: &PgPool, expected: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'")
                .fetch_one(pool).await.unwrap();
            if blocked >= expected { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("writers must overlap at database locks");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn rename_racing_add_member_preserves_unique_names(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let original = f.hire("Original", json!("writer")).await;
    let outsider = f.hire("Ｗｒｉｔｅｒ", json!("writer")).await;
    let team = f.team(&lead, std::slice::from_ref(&original)).await;
    let rename = format!("dots/{}", id(&original));
    let add = format!("teams/{}/members/{}", id(&team["team"]), id(&outsider));
    let lock = lock_workspace(&pool, f.workspace).await;
    let (renamed, added, ()) = tokio::join!(
        f.call("PATCH", &rename, Some(json!({"name":"Writer"}))),
        f.call("PUT", &add, Some(json!({"is_lead":false}))),
        async {
            wait_for_blocked_writers(&pool, 2).await;
            lock.rollback().await.unwrap();
        }
    );
    match (renamed.status().as_u16(), added.status().as_u16()) {
        (200, 422) => assert_code(added, 422, "dot_name_conflict").await,
        (422, 204) => assert_code(renamed, 422, "dot_name_conflict").await,
        statuses => panic!("unexpected race outcomes: {statuses:?}"),
    }
    let view = body(
        f.call("GET", &format!("teams/{}", id(&team["team"])), None)
            .await,
    )
    .await;
    let names: Vec<_> = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dot| dot["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|name| ["Writer", "Ｗｒｉｔｅｒ"].contains(name))
            .count(),
        1
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_second_leads_fail_with_database_backstop(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let first = f.hire("First", json!("writer")).await;
    let second = f.hire("Second", json!("writer")).await;
    let team = f.team(&lead, &[first.clone(), second.clone()]).await;
    let prefix = format!("teams/{}/members", id(&team["team"]));
    let first_path = format!("{prefix}/{}", id(&first));
    let second_path = format!("{prefix}/{}", id(&second));
    let lock = lock_workspace(&pool, f.workspace).await;
    let (a, b, ()) = tokio::join!(
        f.call("PUT", &first_path, Some(json!({"is_lead":true}))),
        f.call("PUT", &second_path, Some(json!({"is_lead":true}))),
        async {
            wait_for_blocked_writers(&pool, 2).await;
            lock.rollback().await.unwrap();
        }
    );
    for response in [a, b] {
        assert_code(response, 422, "team_lead_conflict").await;
    }
    let error = sqlx::query("UPDATE team_members SET is_lead=true WHERE team_id=$1 AND dot_id=$2")
        .bind(Uuid::parse_str(id(&team["team"])).unwrap())
        .bind(Uuid::parse_str(id(&first)).unwrap())
        .execute(&pool)
        .await
        .unwrap_err();
    let db = error.as_database_error().unwrap();
    assert_eq!(db.code().as_deref(), Some("23505"));
    assert_eq!(db.constraint(), Some("team_members_one_lead"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn mixed_concurrent_writers_do_not_deadlock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let dot = f.hire("Writer", json!("writer")).await;
    let second = space(&pool, f.workspace).await;
    let lead = body(
        f.call(
            "POST",
            &format!("spaces/{second}/dots"),
            Some(json!({"name":"Lead","role":"lead","persona":""})),
        )
        .await,
    )
    .await;
    let team = body(
        f.call(
            "POST",
            &format!("spaces/{second}/teams"),
            Some(json!({"name":"Other team", "lead":lead["id"], "members":[]})),
        )
        .await,
    )
    .await;
    let grant = format!("dots/{}/spaces/{second}", id(&dot));
    let member = format!("teams/{}/members/{}", id(&team["team"]), id(&dot));
    let path = format!("dots/{}", id(&dot));
    let lock = lock_workspace(&pool, f.workspace).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            async {
                for _ in 0..5 {
                    assert_eq!(f.call("PUT", &grant, None).await.status(), 204);
                    let response = f.call("DELETE", &grant, None).await;
                    if response.status() != 204 {
                        assert_code(response, 422, "grant_in_use").await;
                    }
                }
            },
            async {
                for i in 0..10 {
                    assert_eq!(
                        f.call(
                            "PATCH",
                            &path,
                            Some(json!({"status":if i%2==0 { "paused" } else { "active" }}))
                        )
                        .await
                        .status(),
                        200
                    );
                }
            },
            async {
                for _ in 0..5 {
                    let response = f.call("PUT", &member, Some(json!({"is_lead":false}))).await;
                    if response.status() != 204 {
                        assert_code(response, 422, "dot_not_in_space").await;
                    }
                    assert_eq!(f.call("DELETE", &member, None).await.status(), 204);
                }
            },
            async {
                wait_for_blocked_writers(&pool, 3).await;
                lock.rollback().await.unwrap();
            }
        );
    })
    .await
    .expect("mixed writers must complete without deadlock");
    let invalid: i64 = sqlx::query_scalar("SELECT count(*) FROM team_members m JOIN teams t ON t.id=m.team_id WHERE m.active AND NOT EXISTS (SELECT 1 FROM dot_spaces s WHERE s.workspace_id=m.workspace_id AND s.dot_id=m.dot_id AND s.space_id=t.space_id)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(invalid, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_seed_demo_creates_exactly_one_roster(pool: PgPool) {
    let mut config = config(None);
    config.demo = true;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(3000003)")
        .execute(&mut *lock)
        .await
        .unwrap();
    let (a, b, ()) = tokio::join!(
        auth::seed_demo(&pool, &config, false),
        auth::seed_demo(&pool, &config, false),
        async {
            wait_for_blocked_writers(&pool, 2).await;
            lock.rollback().await.unwrap();
        }
    );
    assert_eq!(
        [a.unwrap(), b.unwrap()]
            .iter()
            .filter(|password| password.is_some())
            .count(),
        1
    );
    for (table, expected) in [
        ("SELECT count(*) FROM users", 1),
        ("SELECT count(*) FROM workspaces", 1),
        ("SELECT count(*) FROM memberships", 1),
        ("SELECT count(*) FROM spaces", 1),
        ("SELECT count(*) FROM dots", 6),
        ("SELECT count(*) FROM dot_spaces", 6),
        ("SELECT count(*) FROM teams", 1),
        ("SELECT count(*) FROM team_members", 6),
    ] {
        let count: i64 = sqlx::query_scalar(table).fetch_one(&pool).await.unwrap();
        assert_eq!(count, expected, "{table}");
    }
    let leads: i64 =
        sqlx::query_scalar("SELECT count(*) FROM team_members WHERE active AND is_lead")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(leads, 1);
}
