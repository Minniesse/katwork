use super::*;
use katwork::{
    ids::{DotId, SpaceId, UserId, WorkspaceId},
    team::{Autonomy, Dot, DotRole, dot, roles, team},
    tenancy,
    types::{Locale, ModelTier},
};

#[sqlx::test(migrations = "../katwork/migrations")]
async fn repository_dot_update_and_grants_enforce_member_permissions(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let id: DotId = serde_json::from_value(hired["id"].clone()).unwrap();
    let second = SpaceId(space(&pool, f.workspace).await);
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
    let member = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    for patch in &patches {
        assert_eq!(
            dot::update(
                &pool,
                &member,
                id,
                serde_json::from_value(patch.clone()).unwrap()
            )
            .await
            .unwrap_err()
            .code(),
            "forbidden"
        );
    }
    assert_eq!(
        dot::grant_space(&pool, &member, id, second)
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(
        serde_json::to_value(dot::get(&pool, &member, id).await.unwrap()).unwrap(),
        hired
    );
    for role in ["admin", "owner"] {
        f.role(&pool, role).await;
        let admin = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
            .await
            .unwrap();
        for patch in &patches {
            let updated = dot::update(
                &pool,
                &admin,
                id,
                serde_json::from_value(patch.clone()).unwrap(),
            )
            .await
            .unwrap();
            for (field, expected) in patch.as_object().unwrap() {
                assert_eq!(&serde_json::to_value(&updated).unwrap()[field], expected);
            }
        }
        dot::grant_space(&pool, &admin, id, second).await.unwrap();
        let before = serde_json::to_value(dot::get(&pool, &member, id).await.unwrap()).unwrap();
        assert_eq!(
            dot::revoke_space(&pool, &member, id, second)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
        assert_eq!(
            serde_json::to_value(dot::get(&pool, &member, id).await.unwrap()).unwrap(),
            before
        );
        dot::revoke_space(&pool, &admin, id, second).await.unwrap();
        assert_eq!(
            dot::get(&pool, &admin, id).await.unwrap().spaces,
            [SpaceId(f.space)]
        );
        dot::update(&pool, &admin, id, serde_json::from_value(json!({"instructions":hired["instructions"], "tool_allowlist":hired["tool_allowlist"], "model_tier":"standard", "autonomy":"ask_first", "status":"active", "max_concurrency":1})).unwrap()).await.unwrap();
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cosmetic_and_persona_edits_persist_after_reload(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let original: Dot = serde_json::from_value(f.hire("Lead", json!("lead")).await).unwrap();
    f.role(&pool, "member").await;
    let path = format!("dots/{}", original.id);
    for (patch, hash_changes) in [
        (
            json!({"name":"Chief", "appearance":{"avatar":"pen","accent":"red"}}),
            false,
        ),
        (json!({"persona":"Patient and precise"}), true),
    ] {
        let response = f.call("PATCH", &path, Some(patch)).await;
        assert_eq!(response.status(), 200);
        let changed: Dot = serde_json::from_value(body(response).await).unwrap();
        let reloaded = body(
            request(
                &app(&pool, None),
                "GET",
                &format!("/api/workspaces/{}/{path}", f.workspace),
                None,
                Some(&f.cookie),
            )
            .await,
        )
        .await;
        assert_eq!(reloaded, serde_json::to_value(&changed).unwrap());
        assert_eq!(changed.policy_version, 1);
        assert_eq!(
            original.config_hash() != changed.config_hash(),
            hash_changes
        );
        let stored: (String, String, Value, i64) =
            sqlx::query_as("SELECT name,persona,appearance,policy_version FROM dots WHERE id=$1")
                .bind(original.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            stored,
            (
                "Chief".into(),
                if hash_changes {
                    "Patient and precise"
                } else {
                    "Careful"
                }
                .into(),
                json!({"avatar":"pen","accent":"red"}),
                1
            )
        );
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn default_roster_failure_rolls_back_all_rows(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE roster_attempts; CREATE FUNCTION fail_fourth_dot() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('roster_attempts') = 4 THEN RAISE EXCEPTION 'injected roster failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_roster BEFORE INSERT ON dots FOR EACH ROW EXECUTE FUNCTION fail_fourth_dot();")
        .execute(&pool).await.unwrap();
    assert_eq!(
        team::create_default(&pool, &tenant, SpaceId(f.space), Locale::En)
            .await
            .unwrap_err()
            .code(),
        "database_error"
    );
    let attempted: i64 = sqlx::query_scalar("SELECT last_value FROM roster_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempted, 4);
    for table in [
        "SELECT count(*) FROM dots",
        "SELECT count(*) FROM dot_spaces",
        "SELECT count(*) FROM teams",
        "SELECT count(*) FROM team_members",
    ] {
        let count: i64 = sqlx::query_scalar(table).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "{table}");
    }
    sqlx::query("DROP TRIGGER fail_roster ON dots")
        .execute(&pool)
        .await
        .unwrap();
    let team = team::create_default(&pool, &tenant, SpaceId(f.space), Locale::En)
        .await
        .unwrap();
    assert_eq!(
        team::get(&pool, &tenant, team).await.unwrap().members.len(),
        6
    );
    for (table, expected) in [
        ("SELECT count(*) FROM dots", 6),
        ("SELECT count(*) FROM dot_spaces", 6),
        ("SELECT count(*) FROM teams", 1),
        ("SELECT count(*) FROM team_members", 6),
    ] {
        let count: i64 = sqlx::query_scalar(table).fetch_one(&pool).await.unwrap();
        assert_eq!(count, expected, "{table}");
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn removed_membership_is_retained_and_reactivated_in_order(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let first = f.hire("First", json!("writer")).await;
    let last = f.hire("Last", json!("writer")).await;
    let team = f.team(&lead, &[first.clone(), last.clone()]).await;
    let path = format!("teams/{}", id(&team["team"]));
    let member_path = format!("{path}/members/{}", id(&first));
    for _ in 0..2 {
        assert_eq!(f.call("DELETE", &member_path, None).await.status(), 204);
    }
    let row: (bool, bool, i32) = sqlx::query_as(
        "SELECT active,is_lead,position FROM team_members WHERE team_id=$1 AND dot_id=$2",
    )
    .bind(Uuid::parse_str(id(&team["team"])).unwrap())
    .bind(Uuid::parse_str(id(&first)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (false, false, 1));
    // A historical inactive lead must not regain the lead flag when re-added as a member.
    sqlx::query("UPDATE team_members SET is_lead=true WHERE team_id=$1 AND dot_id=$2")
        .bind(Uuid::parse_str(id(&team["team"])).unwrap())
        .bind(Uuid::parse_str(id(&first)).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        body(f.call("GET", &path, None).await).await["members"],
        json!([lead, last])
    );
    for _ in 0..2 {
        assert_eq!(
            f.call("PUT", &member_path, Some(json!({"is_lead":false})))
                .await
                .status(),
            204
        );
    }
    let rows: Vec<(bool, bool, i32)> = sqlx::query_as(
        "SELECT active,is_lead,position FROM team_members WHERE team_id=$1 ORDER BY position",
    )
    .bind(Uuid::parse_str(id(&team["team"])).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, [(true, true, 0), (true, false, 1), (true, false, 2)]);
    assert_eq!(
        body(f.call("GET", &path, None).await).await["members"],
        json!([lead, first, last])
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn rename_ignores_soft_removed_teammates(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let writer = f.hire("Writer", json!("writer")).await;
    let other = f.hire("Other", json!("writer")).await;
    let team = f.team(&lead, &[writer.clone(), other.clone()]).await;
    let member = format!("teams/{}/members/{}", id(&team["team"]), id(&writer));
    assert_eq!(f.call("DELETE", &member, None).await.status(), 204);
    assert_eq!(
        f.call(
            "PATCH",
            &format!("dots/{}", id(&other)),
            Some(json!({"name":"WRITER"}))
        )
        .await
        .status(),
        200
    );
    assert_code(
        f.call("PUT", &member, Some(json!({"is_lead":false}))).await,
        422,
        "dot_name_conflict",
    )
    .await;
    assert_eq!(
        f.call(
            "PATCH",
            &format!("dots/{}", id(&writer)),
            Some(json!({"name":"LEAD"}))
        )
        .await
        .status(),
        200
    );
    assert_code(
        f.call("PUT", &member, Some(json!({"is_lead":false}))).await,
        422,
        "dot_name_conflict",
    )
    .await;
}

#[test]
fn every_role_template_has_safe_defaults() {
    for (role, en, th, avatar, instruction) in [
        (
            DotRole::Lead,
            "Lead",
            "หัวหน้าทีม",
            "compass",
            "Coordinate the Dots and clarify the user's goal.",
        ),
        (
            DotRole::Researcher,
            "Researcher",
            "นักวิจัย",
            "magnifier",
            "Gather research, cite sources, and flag uncertainty.",
        ),
        (
            DotRole::Writer,
            "Writer",
            "นักเขียน",
            "pen",
            "Write clear drafts suited to the user's audience.",
        ),
        (
            DotRole::Analyst,
            "Analyst",
            "นักวิเคราะห์",
            "chart",
            "Analyze data, check assumptions, and explain findings.",
        ),
        (
            DotRole::Scheduler,
            "Scheduler",
            "เลขา",
            "calendar",
            "Organize schedules and clarify timing and dependencies.",
        ),
        (
            DotRole::Reviewer,
            "Reviewer",
            "ผู้ตรวจ",
            "check",
            "Review work for accuracy, completeness, and risks.",
        ),
        (
            DotRole::Custom("HR".into()),
            "HR",
            "HR",
            "dot",
            "Help with the user's task and ask when clarification is needed.",
        ),
    ] {
        for (locale, name) in [(Locale::En, en), (Locale::Th, th)] {
            let new = roles::template(&role, locale);
            assert_eq!(new.name, name);
            assert_eq!(new.role, role);
            assert_eq!(new.instructions, instruction);
            assert!(new.persona.is_empty());
            assert_eq!(new.tool_allowlist, ["ask_user", "read_thread"]);
            assert_eq!(new.model_tier, ModelTier::Standard);
            assert_eq!(new.autonomy, Autonomy::AskFirst);
            assert_eq!(new.appearance.avatar, avatar);
            assert_eq!(new.appearance.accent, "blue");
        }
    }
}
