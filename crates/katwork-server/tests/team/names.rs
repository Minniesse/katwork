use super::*;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn unconverged_names_are_rejected_in_hire_rename_and_team_create(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Writer", json!("writer")).await;
    let name = format!("ก\u{0e38}{}\u{0e31}", "\u{0e31}\u{ff9e}".repeat(5));
    for (method, path, payload) in [
        (
            "POST",
            format!("spaces/{}/kats", f.space),
            json!({"name":name,"role":"writer","persona":""}),
        ),
        ("PATCH", format!("kats/{}", id(&kat)), json!({"name":name})),
        (
            "POST",
            format!("spaces/{}/teams", f.space),
            json!({"name":name,"lead":kat["id"],"members":[]}),
        ),
    ] {
        assert_code(
            f.call(method, &path, Some(payload)).await,
            400,
            "request_invalid",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn mixed_thai_mark_spellings_conflict_in_team_create_and_rename(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (name, folded) in [
        (
            "ก\u{0e38}\u{0e31}\u{3099}\u{0e31}",
            "ก\u{0e31}\u{3099}\u{0e31}\u{0e38}",
        ),
        (
            "ก\u{0e38}\u{0e34}\u{3099}\u{0e34}",
            "ก\u{0e34}\u{3099}\u{0e34}\u{0e38}",
        ),
    ] {
        let lead = f.hire(name, json!("lead")).await;
        let duplicate = f.hire(folded, json!("writer")).await;
        assert_eq!(lead["name"], name);
        assert_eq!(duplicate["name"], folded);
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/teams", f.space),
                Some(json!({"name":"Team","lead":lead["id"],"members":[duplicate["id"]]})),
            )
            .await,
            422,
            "kat_name_conflict",
        )
        .await;
        let writer = f.hire("Writer", json!("writer")).await;
        f.team(&lead, std::slice::from_ref(&writer)).await;
        assert_code(
            f.call(
                "PATCH",
                &format!("kats/{}", id(&writer)),
                Some(json!({"name":folded})),
            )
            .await,
            422,
            "kat_name_conflict",
        )
        .await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn names_allow_at_most_four_combining_marks_per_base(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Writer", json!("writer")).await;
    for name in [
        format!("a{}", "\u{301}".repeat(5)),
        format!("é{}", "\u{301}".repeat(4)),
        format!("ก{}", "\u{e49}".repeat(5)),
    ] {
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/kats", f.space),
                Some(json!({"name":name,"role":"writer","persona":""})),
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
    }
    for name in [
        format!("a{}", "\u{301}".repeat(4)),
        format!("é{}", "\u{301}".repeat(3)),
        format!("ก{}", "\u{e49}".repeat(4)),
        format!("a{}b{}", "\u{301}".repeat(4), "\u{301}".repeat(4)),
    ] {
        assert_eq!(f.hire(&name, json!("writer")).await["name"], name);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn unicode_17_names_are_rejected_in_hire_rename_and_membership_writes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let kat = f.hire("Writer", json!("writer")).await;
    let team = f.team(&lead, &[]).await;
    for name in ["Writer\u{a7ce}", "Writer\u{a7cf}"] {
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/kats", f.space),
                Some(json!({"name":name,"role":"writer","persona":""})),
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
        // Existing rows from the former Unicode 17 validator must not bypass team-write validation.
        sqlx::query("UPDATE kats SET name=$2 WHERE id=$1")
            .bind(Uuid::parse_str(id(&kat)).unwrap())
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        assert_code(
            f.call(
                "POST",
                &format!("spaces/{}/teams", f.space),
                Some(json!({"name":"Team","lead":lead["id"],"members":[kat["id"]]})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
        assert_code(
            f.call(
                "PUT",
                &format!("teams/{}/members/{}", id(&team["team"]), id(&kat)),
                Some(json!({"is_lead":false})),
            )
            .await,
            400,
            "request_invalid",
        )
        .await;
        sqlx::query("UPDATE kats SET name='Writer' WHERE id=$1")
            .bind(Uuid::parse_str(id(&kat)).unwrap())
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn blank_invisible_and_unassigned_names_are_rejected(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    for name in [
        "\u{3164}",
        "\u{2800}",
        "\u{1160}",
        "\u{e49}",
        "\u{301}",
        "\u{034f}",
        "\u{fe0f}",
        "\u{e0100}",
        "\u{e000}",
        "\u{f0000}",
        "\u{378}",
        "\u{a0}",
        "---",
        "🎨",
        "\u{301}A",
        "\u{e49}ก",
        "A\u{3164}B",
        "A\u{2800}B",
        "A\u{1160}B",
        "A\u{034f}B",
        "A\u{fe0f}B",
        "A\u{e0100}B",
        "A\u{e000}B",
        "A\u{378}B",
        "A\u{115f}B",
        "A\u{17b4}B",
        "A\u{180f}B",
        "A\u{ffa0}B",
    ] {
        for (method, path, payload) in [
            (
                "POST",
                format!("spaces/{}/kats", f.space),
                json!({"name":name,"role":"writer","persona":""}),
            ),
            ("PATCH", format!("kats/{}", id(&lead)), json!({"name":name})),
            (
                "POST",
                format!("spaces/{}/teams", f.space),
                json!({"name":name,"lead":lead["id"],"members":[]}),
            ),
        ] {
            let response = f.call(method, &path, Some(payload)).await;
            assert_eq!(
                response.status(),
                400,
                "name {name:?}: {}",
                body(response).await
            );
        }
    }
    for name in ["นักวิจัย", "Café", "๑๒๓", "A 🎨", "ปี่", "นํา"]
    {
        assert_eq!(f.hire(name, json!("writer")).await["name"], name);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn cyrillic_lookalikes_remain_distinct_without_confusable_skeletons(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let latin = f.hire("Writer", json!("lead")).await;
    let cyrillic = f.hire("Wrіter", json!("writer")).await;
    let team = f.team(&latin, std::slice::from_ref(&cyrillic)).await;
    assert_eq!(team["members"], json!([latin, cyrillic]));
}
