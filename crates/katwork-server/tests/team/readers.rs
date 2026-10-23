#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;
use katwork::{
    ids::{KatId, SpaceId, TeamId, UserId, WorkspaceId},
    team::{kat, team},
    tenancy,
};

#[sqlx::test(migrations = "../katwork/migrations")]
async fn connection_readers_filter_workspace_status_membership_and_grants(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let lead = f.hire("Lead", json!("lead")).await;
    let valid = f.hire("Writer", json!("writer")).await;
    let paused = f.hire("Paused", json!("writer")).await;
    let removed = f.hire("Removed", json!("writer")).await;
    let revoked = f.hire("Revoked", json!("writer")).await;
    let second = SpaceId(space(&pool, f.workspace).await);
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    let ids: Vec<KatId> = [&lead, &valid, &paused, &removed, &revoked]
        .into_iter()
        .map(|kat| serde_json::from_value(kat["id"].clone()).unwrap())
        .collect();
    for id in &ids {
        kat::grant_space(
            &pool,
            &tenant,
            *id,
            second,
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();
    }
    let team_id = team::create(
        &pool,
        &tenant,
        second,
        katwork::team::NewTeam {
            name: "Other team".into(),
            lead: ids[0],
            members: ids[1..].to_vec(),
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        f.call(
            "PATCH",
            &format!("kats/{}", ids[2]),
            Some(json!({"status":"paused"}))
        )
        .await
        .status(),
        200
    );
    team::remove_member(
        &pool,
        &tenant,
        team_id,
        ids[3],
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    // Simulate a legacy membership whose grant was revoked before the grant-in-use guard existed.
    sqlx::query("DELETE FROM kat_spaces WHERE workspace_id=$1 AND kat_id=$2 AND space_id=$3")
        .bind(f.workspace)
        .bind(ids[4])
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let eligible = team::eligible_members(&mut tx, &tenant, team_id)
        .await
        .unwrap();
    assert_eq!(
        eligible.iter().map(|kat| kat.id).collect::<Vec<_>>(),
        ids[..2]
    );
    for (id, expected) in ids.iter().zip([true, true, true, true, false]) {
        assert_eq!(
            kat::is_granted(&mut tx, &tenant, *id, second)
                .await
                .unwrap(),
            expected
        );
    }
    assert!(
        !kat::is_granted(&mut tx, &tenant, KatId::new(), second)
            .await
            .unwrap()
    );
    assert!(
        !kat::is_granted(&mut tx, &tenant, ids[0], SpaceId::new())
            .await
            .unwrap()
    );
    assert_eq!(
        team::eligible_members(&mut tx, &tenant, TeamId::new())
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    sqlx::query("UPDATE kats SET persona='Uncommitted', status='paused' WHERE id=$1")
        .bind(ids[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        kat::get_on(&mut *tx, &tenant, ids[0])
            .await
            .unwrap()
            .persona,
        "Uncommitted"
    );
    assert!(
        kat::is_granted(&mut tx, &tenant, ids[0], second)
            .await
            .unwrap()
    );
    assert_eq!(
        team::eligible_members(&mut tx, &tenant, team_id)
            .await
            .unwrap()
            .iter()
            .map(|kat| kat.id)
            .collect::<Vec<_>>(),
        [ids[1]]
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        kat::get(&pool, &tenant, ids[0]).await.unwrap().persona,
        "Careful"
    );

    let foreign_workspace = workspace(&pool).await;
    let foreign_space = SpaceId(space(&pool, foreign_workspace).await);
    sqlx::query("INSERT INTO memberships (workspace_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(foreign_workspace)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(foreign_workspace))
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        kat::get_on(&mut *conn, &foreign, ids[0])
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert!(
        !kat::is_granted(&mut conn, &foreign, ids[0], second)
            .await
            .unwrap()
    );
    assert!(
        !kat::is_granted(&mut conn, &tenant, ids[0], foreign_space)
            .await
            .unwrap()
    );
    assert_eq!(
        team::eligible_members(&mut conn, &foreign, team_id)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn paused_kats_keep_grants_but_are_not_eligible(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let hired = f.hire("Writer", json!("writer")).await;
    let id: KatId = serde_json::from_value(hired["id"].clone()).unwrap();
    let space = SpaceId(space(&pool, f.workspace).await);
    let tenant = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap();
    kat::grant_space(&pool, &tenant, id, space, std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(kat::is_granted(&mut tx, &tenant, id, space).await.unwrap());
    assert!(kat::is_eligible(&mut tx, &tenant, id, space).await.unwrap());
    sqlx::query("UPDATE kats SET status='paused' WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(kat::is_granted(&mut tx, &tenant, id, space).await.unwrap());
    assert!(!kat::is_eligible(&mut tx, &tenant, id, space).await.unwrap());
    sqlx::query("DELETE FROM kat_spaces WHERE kat_id=$1 AND space_id=$2")
        .bind(id)
        .bind(space)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(!kat::is_granted(&mut tx, &tenant, id, space).await.unwrap());
    assert!(!kat::is_eligible(&mut tx, &tenant, id, space).await.unwrap());
    tx.rollback().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        kat::is_granted(&mut conn, &tenant, id, space)
            .await
            .unwrap()
    );
    assert!(
        kat::is_eligible(&mut conn, &tenant, id, space)
            .await
            .unwrap()
    );
    assert!(
        !kat::is_eligible(&mut conn, &tenant, KatId::new(), space)
            .await
            .unwrap()
    );
    assert!(
        !kat::is_eligible(&mut conn, &tenant, id, SpaceId::new())
            .await
            .unwrap()
    );
    let other = workspace(&pool).await;
    sqlx::query("INSERT INTO memberships (workspace_id,user_id,role) VALUES ($1,$2,'owner')")
        .bind(other)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = tenancy::resolve(&pool, UserId(f.user), WorkspaceId(other))
        .await
        .unwrap();
    assert!(
        !kat::is_eligible(&mut conn, &foreign, id, space)
            .await
            .unwrap()
    );
}
