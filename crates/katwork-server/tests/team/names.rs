use super::*;

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
                format!("spaces/{}/dots", f.space),
                json!({"name":name,"role":"writer","persona":""}),
            ),
            ("PATCH", format!("dots/{}", id(&lead)), json!({"name":name})),
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
