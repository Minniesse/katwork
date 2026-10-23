use super::*;
use katwork::{
    ids::{KatId, SpaceId, UserId, WorkspaceId},
    team::{Autonomy, Kat, KatRole, kat, roles, team},
    tenancy,
    types::{Locale, ModelTier},
};

#[sqlx::test(migrations = "../katwork/migrations")]
async fn repository_kat_update_and_grants_enforce_member_permissions(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let id: KatId = serde_json::from_value(hired["id"].clone()).unwrap();
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
            kat::update(
                &pool,
                &member,
                id,
                serde_json::from_value(patch.clone()).unwrap(),
                std::time::Duration::from_secs(5)
            )
            .await
            .unwrap_err()
            .code(),
            "forbidden"
        );
    }
    assert_eq!(
        kat::grant_space(
            &pool,
            &member,
            id,
            second,
            std::time::Duration::from_secs(5)
        )
        .await
        .unwrap_err()
        .code(),
        "forbidden"
    );
    assert_eq!(
        serde_json::to_value(kat::get(&pool, &member, id).await.unwrap()).unwrap(),
        hired
    );
    for role in ["admin", "owner"] {
        f.role(&pool, role).await;
        let admin = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
            .await
            .unwrap();
        for patch in &patches {
            let updated = kat::update(
                &pool,
                &admin,
                id,
                serde_json::from_value(patch.clone()).unwrap(),
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap();
            for (field, expected) in patch.as_object().unwrap() {
                assert_eq!(&serde_json::to_value(&updated).unwrap()[field], expected);
            }
        }
        kat::grant_space(&pool, &admin, id, second, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        let before = serde_json::to_value(kat::get(&pool, &member, id).await.unwrap()).unwrap();
        assert_eq!(
            kat::revoke_space(
                &pool,
                &member,
                id,
                second,
                std::time::Duration::from_secs(5)
            )
            .await
            .unwrap_err()
            .code(),
            "forbidden"
        );
        assert_eq!(
            serde_json::to_value(kat::get(&pool, &member, id).await.unwrap()).unwrap(),
            before
        );
        kat::revoke_space(&pool, &admin, id, second, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            kat::get(&pool, &admin, id).await.unwrap().spaces,
            [SpaceId(f.space)]
        );
        kat::update(&pool, &admin, id, serde_json::from_value(json!({"instructions":hired["instructions"], "tool_allowlist":hired["tool_allowlist"], "model_tier":"standard", "autonomy":"ask_first", "status":"active", "max_concurrency":1})).unwrap(), std::time::Duration::from_secs(5)).await.unwrap();
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_cosmetic_and_persona_edits_persist_after_reload(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let original: Kat = serde_json::from_value(f.hire("Lead", json!("lead")).await).unwrap();
    f.role(&pool, "member").await;
    let path = format!("kats/{}", original.id);
    for (patch, hash_changes) in [
        (
            json!({"name":"Chief", "appearance":{"avatar":"pen","accent":"red"}}),
            false,
        ),
        (json!({"persona":"Patient and precise"}), true),
    ] {
        let response = f.call("PATCH", &path, Some(patch)).await;
        assert_eq!(response.status(), 200);
        let changed: Kat = serde_json::from_value(body(response).await).unwrap();
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
            sqlx::query_as("SELECT name,persona,appearance,policy_version FROM kats WHERE id=$1")
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
    sqlx::raw_sql("CREATE SEQUENCE roster_attempts; CREATE FUNCTION fail_fourth_kat() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('roster_attempts') = 4 THEN RAISE EXCEPTION 'injected roster failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_roster BEFORE INSERT ON kats FOR EACH ROW EXECUTE FUNCTION fail_fourth_kat();")
        .execute(&pool).await.unwrap();
    assert_eq!(
        team::create_default(
            &pool,
            &tenant,
            SpaceId(f.space),
            Locale::En,
            std::time::Duration::from_secs(5)
        )
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
        "SELECT count(*) FROM kats",
        "SELECT count(*) FROM kat_spaces",
        "SELECT count(*) FROM teams",
        "SELECT count(*) FROM team_members",
    ] {
        let count: i64 = sqlx::query_scalar(table).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "{table}");
    }
    sqlx::query("DROP TRIGGER fail_roster ON kats")
        .execute(&pool)
        .await
        .unwrap();
    let team = team::create_default(
        &pool,
        &tenant,
        SpaceId(f.space),
        Locale::En,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        team::get(&pool, &tenant, team).await.unwrap().members.len(),
        6
    );
    for (table, expected) in [
        ("SELECT count(*) FROM kats", 6),
        ("SELECT count(*) FROM kat_spaces", 6),
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
        "SELECT active,is_lead,position FROM team_members WHERE team_id=$1 AND kat_id=$2",
    )
    .bind(Uuid::parse_str(id(&team["team"])).unwrap())
    .bind(Uuid::parse_str(id(&first)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (false, false, 1));
    // A historical inactive lead must not regain the lead flag when re-added as a member.
    sqlx::query("UPDATE team_members SET is_lead=true WHERE team_id=$1 AND kat_id=$2")
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
            &format!("kats/{}", id(&other)),
            Some(json!({"name":"WRITER"}))
        )
        .await
        .status(),
        200
    );
    assert_code(
        f.call("PUT", &member, Some(json!({"is_lead":false}))).await,
        422,
        "kat_name_conflict",
    )
    .await;
    assert_eq!(
        f.call(
            "PATCH",
            &format!("kats/{}", id(&writer)),
            Some(json!({"name":"LEAD"}))
        )
        .await
        .status(),
        200
    );
    assert_code(
        f.call("PUT", &member, Some(json!({"is_lead":false}))).await,
        422,
        "kat_name_conflict",
    )
    .await;
}

#[test]
fn every_role_template_has_safe_defaults() {
    for (role, en, th, avatar, instruction) in [
        (
            KatRole::Lead,
            "Lead",
            "หัวหน้าทีม",
            "compass",
            "Coordinate the Kats and clarify the user's goal.",
        ),
        (
            KatRole::Researcher,
            "Researcher",
            "นักวิจัย",
            "magnifier",
            "Gather research, cite sources, and flag uncertainty.",
        ),
        (
            KatRole::Writer,
            "Writer",
            "นักเขียน",
            "pen",
            "Write clear drafts suited to the user's audience.",
        ),
        (
            KatRole::Analyst,
            "Analyst",
            "นักวิเคราะห์",
            "chart",
            "Analyze data, check assumptions, and explain findings.",
        ),
        (
            KatRole::Scheduler,
            "Scheduler",
            "เลขา",
            "calendar",
            "Organize schedules and clarify timing and dependencies.",
        ),
        (
            KatRole::Reviewer,
            "Reviewer",
            "ผู้ตรวจ",
            "check",
            "Review work for accuracy, completeness, and risks.",
        ),
        (
            KatRole::Custom("HR".into()),
            "HR",
            "HR",
            "kat",
            "Help with the user's task and ask when clarification is needed.",
        ),
    ] {
        for (locale, name, reply) in [(Locale::En, en, REPLY_EN), (Locale::Th, th, REPLY_TH)] {
            let new = roles::template(&role, locale);
            assert_eq!(new.name, name);
            assert_eq!(new.role, role);
            assert_eq!(new.instructions, format!("{instruction} {reply}"));
            assert_eq!(
                new.max_concurrency,
                if role == KatRole::Lead { 3 } else { 1 }
            );
            assert!(new.persona.is_empty());
            assert_eq!(new.tool_allowlist, ["ask_user", "read_thread"]);
            assert_eq!(new.model_tier, ModelTier::Standard);
            assert_eq!(new.autonomy, Autonomy::AskFirst);
            assert_eq!(new.appearance.avatar, avatar);
            assert_eq!(new.appearance.accent, "blue");
        }
    }
}

const REPLY_EN: &str = "Reply in the language of the person's latest message.";
const REPLY_TH: &str = "ตอบเป็นภาษาเดียวกับข้อความล่าสุดของคนที่คุยด้วย";

fn every_role() -> [KatRole; 7] {
    [
        KatRole::Lead,
        KatRole::Researcher,
        KatRole::Writer,
        KatRole::Analyst,
        KatRole::Scheduler,
        KatRole::Reviewer,
        KatRole::Custom("HR".into()),
    ]
}

#[test]
fn role_templates_tell_kats_to_reply_in_the_persons_language() {
    for role in every_role() {
        for (locale, line) in [(Locale::En, REPLY_EN), (Locale::Th, REPLY_TH)] {
            let instructions = roles::template(&role, locale).instructions;
            assert!(instructions.ends_with(line), "{role:?} {locale:?}");
            assert_eq!(instructions.matches(line).count(), 1, "{role:?} {locale:?}");
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_can_hire_from_either_locales_template_but_not_change_it(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let member = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let space = SpaceId(f.space);
    // Hiring proves each template, reply line included, passes the free-text checks.
    for role in every_role() {
        for locale in [Locale::En, Locale::Th] {
            let template = roles::template(&role, locale);
            let id = kat::hire(
                &pool,
                &member,
                space,
                template.clone(),
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap();
            let kat = kat::get(&pool, &member, id).await.unwrap();
            assert_eq!(kat.instructions, template.instructions);
            assert_eq!(kat.max_concurrency, template.max_concurrency);
        }
    }
    let mut changed = roles::template(&KatRole::Writer, Locale::Th);
    changed.instructions.push_str(" Skip the review.");
    let mut lead = roles::template(&KatRole::Lead, Locale::En);
    lead.max_concurrency = 1;
    let mut writer = roles::template(&KatRole::Writer, Locale::En);
    writer.max_concurrency = 3;
    for new in [changed, lead, writer] {
        assert_eq!(
            kat::hire(
                &pool,
                &member,
                space,
                new.clone(),
                std::time::Duration::from_secs(5)
            )
            .await
            .unwrap_err()
            .code(),
            "forbidden",
            "{new:?}"
        );
    }
    let kats: i64 = sqlx::query_scalar("SELECT count(*) FROM kats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kats, 14);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn admin_may_hire_with_any_positive_max_concurrency(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let admin = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let mut writer = roles::template(&KatRole::Writer, Locale::En);
    writer.max_concurrency = 2;
    let id = kat::hire(
        &pool,
        &admin,
        SpaceId(f.space),
        writer.clone(),
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        kat::get(&pool, &admin, id).await.unwrap().max_concurrency,
        2
    );
    writer.max_concurrency = 0;
    assert_eq!(
        kat::hire(
            &pool,
            &admin,
            SpaceId(f.space),
            writer,
            std::time::Duration::from_secs(5)
        )
        .await
        .unwrap_err()
        .code(),
        "request_invalid"
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn member_can_create_the_thai_default_team(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.role(&pool, "member").await;
    let member = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let id = team::create_default(
        &pool,
        &member,
        SpaceId(f.space),
        Locale::Th,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    let view = team::get(&pool, &member, id).await.unwrap();
    assert_eq!(view.team.name, "ทีมเริ่มต้น");
    let names: Vec<_> = view.members.iter().map(|kat| kat.name.as_str()).collect();
    assert_eq!(
        names,
        ["หัวหน้าทีม", "นักวิจัย", "นักเขียน", "นักวิเคราะห์", "เลขา", "ผู้ตรวจ"]
    );
    assert!(
        view.members
            .iter()
            .all(|kat| kat.instructions.ends_with(REPLY_TH))
    );
}
