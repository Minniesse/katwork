use super::*;
use katwork::{
    ids::{DotId, SpaceId, TeamId, UserId, WorkspaceId},
    team::{dot, team},
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
    let ids: Vec<DotId> = [&lead, &valid, &paused, &removed, &revoked]
        .into_iter()
        .map(|dot| serde_json::from_value(dot["id"].clone()).unwrap())
        .collect();
    for id in &ids {
        dot::grant_space(&pool, &tenant, *id, second).await.unwrap();
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
    )
    .await
    .unwrap();
    assert_eq!(
        f.call(
            "PATCH",
            &format!("dots/{}", ids[2]),
            Some(json!({"status":"paused"}))
        )
        .await
        .status(),
        200
    );
    team::remove_member(&pool, &tenant, team_id, ids[3])
        .await
        .unwrap();
    // Simulate a legacy membership whose grant was revoked before the grant-in-use guard existed.
    sqlx::query("DELETE FROM dot_spaces WHERE workspace_id=$1 AND dot_id=$2 AND space_id=$3")
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
        eligible.iter().map(|dot| dot.id).collect::<Vec<_>>(),
        ids[..2]
    );
    for (id, expected) in ids.iter().zip([true, true, false, true, false]) {
        assert_eq!(
            dot::is_granted(&mut tx, &tenant, *id, second)
                .await
                .unwrap(),
            expected
        );
    }
    assert!(
        !dot::is_granted(&mut tx, &tenant, DotId::new(), second)
            .await
            .unwrap()
    );
    assert!(
        !dot::is_granted(&mut tx, &tenant, ids[0], SpaceId::new())
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
    sqlx::query("UPDATE dots SET persona='Uncommitted', status='paused' WHERE id=$1")
        .bind(ids[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        dot::get_on(&mut *tx, &tenant, ids[0])
            .await
            .unwrap()
            .persona,
        "Uncommitted"
    );
    assert!(
        !dot::is_granted(&mut tx, &tenant, ids[0], second)
            .await
            .unwrap()
    );
    assert_eq!(
        team::eligible_members(&mut tx, &tenant, team_id)
            .await
            .unwrap()
            .iter()
            .map(|dot| dot.id)
            .collect::<Vec<_>>(),
        [ids[1]]
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        dot::get(&pool, &tenant, ids[0]).await.unwrap().persona,
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
        dot::get_on(&mut *conn, &foreign, ids[0])
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert!(
        !dot::is_granted(&mut conn, &foreign, ids[0], second)
            .await
            .unwrap()
    );
    assert!(
        !dot::is_granted(&mut conn, &tenant, ids[0], foreign_space)
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
