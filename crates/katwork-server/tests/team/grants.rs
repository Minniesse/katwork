use super::*;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn revoke_refuses_active_member_and_lead_grants(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let writer = f.hire("Writer", json!("writer")).await;
    let second = space(&pool, f.workspace).await;
    for kat in [&lead, &writer] {
        assert_eq!(
            f.call("PUT", &format!("kats/{}/spaces/{second}", id(kat)), None)
                .await
                .status(),
            204
        );
    }
    let response = f
        .call(
            "POST",
            &format!("spaces/{second}/teams"),
            Some(json!({"name":"Other team", "lead":lead["id"], "members":[writer["id"]]})),
        )
        .await;
    assert_eq!(response.status(), 201);
    let team = body(response).await;
    for kat in [&lead, &writer] {
        let before = body(f.call("GET", &format!("kats/{}", id(kat)), None).await).await;
        assert_code(
            f.call("DELETE", &format!("kats/{}/spaces/{second}", id(kat)), None)
                .await,
            422,
            "grant_in_use",
        )
        .await;
        assert_eq!(
            body(f.call("GET", &format!("kats/{}", id(kat)), None).await).await,
            before
        );
    }
    assert_eq!(
        f.call(
            "DELETE",
            &format!("teams/{}/members/{}", id(&team["team"]), id(&writer)),
            None
        )
        .await
        .status(),
        204
    );
    assert_eq!(
        f.call(
            "DELETE",
            &format!("kats/{}/spaces/{second}", id(&writer)),
            None
        )
        .await
        .status(),
        204
    );
    assert_eq!(
        body(f.call("GET", &format!("kats/{}", id(&writer)), None).await).await["policy_version"],
        3
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn kat_list_follows_non_default_grants(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let kat = f.hire("Lead", json!("lead")).await;
    let second = space(&pool, f.workspace).await;
    let grant = format!("kats/{}/spaces/{second}", id(&kat));
    for (method, expected) in [("PUT", 1), ("DELETE", 0)] {
        assert_eq!(f.call(method, &grant, None).await.status(), 204);
        let original = body(
            f.call("GET", &format!("spaces/{}/kats", f.space), None)
                .await,
        )
        .await;
        let other = body(f.call("GET", &format!("spaces/{second}/kats"), None).await).await;
        assert_eq!(original.as_array().unwrap().len(), 1);
        assert_eq!(original[0]["id"], kat["id"]);
        assert_eq!(other.as_array().unwrap().len(), expected);
        if method == "PUT" {
            assert_eq!(other, original);
        }
    }
}
