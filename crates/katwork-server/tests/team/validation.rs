use super::*;
use katwork::{
    ids::{SpaceId, UserId, WorkspaceId},
    team::{Dot, DotRole, dot, roles},
    tenancy,
    types::Locale,
};

#[sqlx::test(migrations = "../katwork/migrations")]
async fn only_policy_fields_bump_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&hired));
    let mut version = 1;
    for patch in [
        json!({"tool_allowlist":["read_thread"]}),
        json!({"status":"paused"}),
        json!({"autonomy":"standard"}),
        json!({"model_tier":"strong"}),
    ] {
        let response = f.call("PATCH", &path, Some(patch.clone())).await;
        assert_eq!(response.status(), 200);
        version += 1;
        assert_eq!(body(response).await["policy_version"], version);
        assert_eq!(
            body(f.call("PATCH", &path, Some(patch)).await).await["policy_version"],
            version
        );
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn next_run_settings_do_not_bump_policy_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&hired));
    for patch in [
        json!({"max_concurrency":2}),
        json!({"daily_token_budget":100}),
        json!({"daily_token_budget":null}),
        json!({"persona":"Patient"}),
        json!({"instructions":"Be concise"}),
        json!({"appearance":{"avatar":"pen", "accent":"blue"}}),
        json!({"name":"Chief"}),
    ] {
        let response = f.call("PATCH", &path, Some(patch.clone())).await;
        assert_eq!(response.status(), 200, "{patch}");
        assert_eq!(body(response).await["policy_version"], 1, "{patch}");
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn free_text_rejects_nul_controls_bidi_and_oversize(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&hired));
    for (field, cap) in [("persona", 4_000), ("instructions", 8_000)] {
        for value in [
            "\0", "\u{7f}", "\u{85}", "\u{202e}", "\u{200b}", "\u{2028}", "\u{2029}",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(["ก".repeat(cap + 1)])
        {
            assert_code(
                f.call("PATCH", &path, Some(json!({field: value}))).await,
                400,
                "request_invalid",
            )
            .await;
            if field == "persona" {
                f.role(&pool, "member").await;
                assert_code(
                    f.call(
                        "POST",
                        &format!("spaces/{}/dots", f.space),
                        Some(json!({"name":"Writer", "role":"writer", "persona":value})),
                    )
                    .await,
                    400,
                    "request_invalid",
                )
                .await;
                assert_code(
                    f.call("PATCH", &path, Some(json!({field: value}))).await,
                    400,
                    "request_invalid",
                )
                .await;
                f.role(&pool, "owner").await;
            }
        }
        for value in [String::new(), "ก".repeat(cap)] {
            let response = f.call("PATCH", &path, Some(json!({field:value}))).await;
            assert_eq!(response.status(), 200);
            assert_eq!(body(response).await[field], value);
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn appearance_accepts_only_known_avatars_and_accents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    let path = format!("dots/{}", id(&hired));
    f.role(&pool, "member").await;
    for field in ["avatar", "accent"] {
        for invalid in [
            String::new(),
            "unknown".into(),
            "\0".into(),
            "\u{202e}".into(),
            "a".repeat(201),
        ] {
            let mut appearance = json!({"avatar":"pen", "accent":"blue"});
            appearance[field] = json!(invalid);
            assert_code(
                f.call("PATCH", &path, Some(json!({"appearance":appearance})))
                    .await,
                400,
                "request_invalid",
            )
            .await;
        }
    }
    for avatar in [
        "compass",
        "magnifier",
        "pen",
        "chart",
        "calendar",
        "check",
        "dot",
    ] {
        for accent in ["blue", "yellow", "red", "green", "purple"] {
            let response = f
                .call(
                    "PATCH",
                    &path,
                    Some(json!({"appearance":{"avatar":avatar,"accent":accent}})),
                )
                .await;
            assert_eq!(response.status(), 200);
        }
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn multiline_persona_and_instructions_accept_lf_cr_and_tab(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let text = "First line\nSecond line\r\n\tIndented ไทย";
    f.role(&pool, "member").await;
    let response = f
        .call(
            "POST",
            &format!("spaces/{}/dots", f.space),
            Some(json!({"name":"Writer","role":"writer","persona":text})),
        )
        .await;
    assert_eq!(response.status(), 201);
    let hired = body(response).await;
    assert_eq!(hired["persona"], text);
    let path = format!("dots/{}", id(&hired));
    let response = f.call("PATCH", &path, Some(json!({"persona":text}))).await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await["persona"], text);
    f.role(&pool, "owner").await;
    let response = f
        .call("PATCH", &path, Some(json!({"instructions":text})))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await["instructions"], text);
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let mut new = roles::template(&DotRole::Writer, Locale::En);
    new.instructions = text.into();
    let id = dot::hire(&pool, &tenant, SpaceId(f.space), new)
        .await
        .unwrap();
    assert_eq!(
        dot::get(&pool, &tenant, id).await.unwrap().instructions,
        text
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn tool_allowlist_validates_ids_and_canonicalizes_order_and_dupes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Lead", json!("lead")).await;
    assert_eq!(hired["tool_allowlist"], json!(["ask_user", "read_thread"]));
    let path = format!("dots/{}", id(&hired));
    for invalid in [
        "".into(),
        "1tool".into(),
        "_tool".into(),
        "Tool".into(),
        "rm -rf /".into(),
        "a-b".into(),
        "x\0".into(),
        "ก".into(),
        "a".repeat(65),
    ] {
        assert_code(
            f.call("PATCH", &path, Some(json!({"tool_allowlist":[invalid]})))
                .await,
            400,
            "request_invalid",
        )
        .await;
    }
    let too_many: Vec<_> = (0..33).map(|i| format!("tool_{i}")).collect();
    assert_code(
        f.call("PATCH", &path, Some(json!({"tool_allowlist":too_many})))
            .await,
        400,
        "request_invalid",
    )
    .await;
    for tools in [
        json!(["read_thread", "ask_user"]),
        json!(["ask_user", "read_thread", "ask_user"]),
    ] {
        let response = f
            .call("PATCH", &path, Some(json!({"tool_allowlist":tools})))
            .await;
        assert_eq!(response.status(), 200);
        let changed: Dot = serde_json::from_value(body(response).await).unwrap();
        let original: Dot = serde_json::from_value(hired.clone()).unwrap();
        assert_eq!(changed.tool_allowlist, ["ask_user", "read_thread"]);
        assert_eq!(changed.policy_version, 1);
        assert_eq!(changed.config_hash(), original.config_hash());
    }
    for tools in [json!(["a", "read.thread_1", "a".repeat(64)]), json!([])] {
        let response = f
            .call("PATCH", &path, Some(json!({"tool_allowlist":tools})))
            .await;
        assert_eq!(response.status(), 200);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn repository_hire_validates_all_free_text_and_allowlist(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let template = roles::template(&DotRole::Writer, Locale::En);
    for field in [
        "persona",
        "instructions",
        "avatar",
        "accent",
        "tool_allowlist",
    ] {
        for invalid in ["\0".to_owned(), "\u{202e}".into(), "a".repeat(8_001)] {
            let mut new = template.clone();
            match field {
                "persona" => new.persona = invalid,
                "instructions" => new.instructions = invalid,
                "avatar" => new.appearance.avatar = invalid,
                "accent" => new.appearance.accent = invalid,
                _ => new.tool_allowlist = vec![invalid],
            }
            assert_eq!(
                dot::hire(&pool, &tenant, SpaceId(f.space), new)
                    .await
                    .unwrap_err()
                    .code(),
                "request_invalid",
                "{field}"
            );
        }
    }
    f.role(&pool, "member").await;
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let mut new = template;
    new.tool_allowlist = vec!["ask_user".into(), "read_thread".into(), "ask_user".into()];
    let hired = dot::hire(&pool, &tenant, SpaceId(f.space), new)
        .await
        .unwrap();
    assert_eq!(
        dot::get(&pool, &tenant, hired)
            .await
            .unwrap()
            .tool_allowlist,
        ["ask_user", "read_thread"]
    );
}
