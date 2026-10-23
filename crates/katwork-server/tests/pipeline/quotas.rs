use super::*;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_budget_exceeded_is_stopped_quota_exceeded(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("hello").await;
    assert_eq!(
        f.status(job).await,
        (Some("stopped".into()), Some("quota_exceeded".into()))
    );
    assert_eq!(f.notices().await[0]["code"], "quota_exceeded");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn retry_after_intake_stop_returns_same_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET daily_token_budget=0 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let a = body(f.post("hello", Some("stop")).await).await;
    let retry = f.post("hello", Some("stop")).await;
    assert_eq!(retry.status(), 200);
    assert_eq!(body(retry).await, a);
    let ids: Vec<JobId> = sqlx::query_scalar("SELECT job_id FROM messages ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|id| json!(id) == a["job_id"]));
    assert_eq!(f.events().await.len(), 4);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn rate_limit_is_independent_per_kat_and_per_sender(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..6 {
        f.submit("hello").await;
    }
    let (_, job) = f.submit("seventh").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("too_many_requests"));
    let (_, job) = f.submit("@Writer hello").await;
    assert!(f.status(job).await.0.is_none());
    let user = UserId::new();
    sqlx::query("INSERT INTO users(id,email,password_hash,display_name,locale) VALUES($1,'second@example.test','unused','Second','en')").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(workspace_id,user_id,role) VALUES($1,$2,'member')")
        .bind(f.tenant.workspace_id())
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let tenant = tenancy::resolve(&pool, user, f.tenant.workspace_id())
        .await
        .unwrap();
    let posted = katwork::pipeline::submit(
        &f.core,
        &tenant,
        f.thread,
        katwork::chat::UserPost {
            body: "hello".into(),
            client_msg_id: None,
        },
    )
    .await
    .unwrap();
    assert!(f.status(posted.job_id).await.0.is_none());
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn per_sender_throttle_window_boundary(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..6 {
        f.submit("hello").await;
    }
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '60 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("allowed").await;
    assert!(f.status(job).await.0.is_none());
    sqlx::query("UPDATE jobs SET created_at=clock_timestamp()-interval '59 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("blocked").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("too_many_requests"));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_budget_counts_charged_tokens_when_usage_is_missing(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    f.provider(FakeProvider::scripted(vec![super::execution::answer(
        "Reply", "Reply",
    )]));
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    let (charged, complete): (i64, bool) =
        sqlx::query_as("SELECT charged_tokens,usage_complete FROM model_calls")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(charged > 0);
    assert!(!complete);
    sqlx::query("UPDATE kats SET daily_token_budget=$2 WHERE id=$1")
        .bind(f.kats[0].id)
        .bind(charged)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("next").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("quota_exceeded"));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn daily_quota_window_excludes_old_charges(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("hello").await;
    f.run(job).await;
    sqlx::query("UPDATE kats SET daily_token_budget=1 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE model_calls SET created_at=clock_timestamp()-interval '24 hours'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("allowed").await;
    assert!(f.status(job).await.0.is_none());
    sqlx::query("UPDATE model_calls SET created_at=clock_timestamp()-interval '23 hours'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("blocked").await;
    assert_eq!(f.status(job).await.1.as_deref(), Some("quota_exceeded"));
}
