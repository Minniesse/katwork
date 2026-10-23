#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
use super::*;

struct LeaseWaitSink<'a> {
    h: &'a Harness,
    blocker: tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Postgres>>>,
    ready: Notify,
}

#[async_trait]
impl EventSink for LeaseWaitSink<'_> {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        let start = matches!(event, AgUiEvent::TextMessageStart { .. });
        let seq = self.h.sink.emit(event).await?;
        if start {
            let mut tx = self.h.pool.begin().await?;
            sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
                .bind(self.h.kat.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE task_leases SET expires_at=clock_timestamp()+interval '1 second' WHERE task_id=$1")
                .bind(self.h.input.task_id).execute(&self.h.pool).await?;
            *self.blocker.lock().await = Some(tx);
            self.ready.notify_one();
        }
        Ok(seq)
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.h.sink.close_run(end, code).await
    }
}

#[sqlx::test]
async fn expired_lease_during_kat_lock_must_not_dispatch_model(pool: PgPool) {
    let h = Harness::new(pool, vec![turn("expired worker sent this", vec![])]).await;
    let sink = LeaseWaitSink {
        h: &h,
        blocker: tokio::sync::Mutex::new(None),
        ready: Notify::new(),
    };
    let release = async {
        sink.ready.notified().await;
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id FROM kats%')")
                .fetch_one(&h.pool).await.unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
        loop {
            let expired: bool = sqlx::query_scalar(
                "SELECT expires_at <= clock_timestamp() FROM task_leases WHERE task_id=$1",
            )
            .bind(h.input.task_id)
            .fetch_one(&h.pool)
            .await
            .unwrap();
            if expired {
                break;
            }
            tokio::task::yield_now().await;
        }
        sink.blocker
            .lock()
            .await
            .take()
            .unwrap()
            .rollback()
            .await
            .unwrap();
    };
    let env = RunEnv {
        pool: &h.pool,
        models: &h.models,
        tools: &h.tools,
        sink: &sink,
    };
    let execute = run::execute(&env, &h.kat, h.input.clone(), &h.limits);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(execute, release)
    })
    .await
    .expect("model authorization reached the Kat lock before lease expiry");
    assert!(
        h.fake.received().is_empty(),
        "expired lease dispatched {} model request(s); result={result:?}",
        h.fake.received().len()
    );
    assert!(matches!(result, Err(RunAbort::LeaseLost)));
}

#[sqlx::test]
async fn expired_lease_during_kat_lock_must_not_authorize_write(pool: PgPool) {
    effects_table(&pool).await;
    let h = Harness::new(pool.clone(), vec![]).await;
    let ctx = ToolCtx {
        tenant: h.input.tenant.clone(),
        thread_id: h.input.thread_id,
        space_id: h.input.space_id,
        kat_id: h.kat.id,
        job_id: h.input.job_id,
        task_id: h.input.task_id,
        run_id: h.input.run_id,
        step: 1,
        tool_call_id: "probe".into(),
        idempotency_key: "lease-probe".into(),
        lease_epoch: h.input.lease_epoch,
        policy_version: h.input.policy_version,
        actor: h.input.actor,
        initiator: Initiator::Person,
        computer: None,
        pool: pool.clone(),
        cancel: h.input.cancel.clone(),
    };
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
        .bind(h.kat.id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE task_leases SET expires_at=clock_timestamp()+interval '1 second' WHERE task_id=$1",
    )
    .bind(h.input.task_id)
    .execute(&pool)
    .await
    .unwrap();
    let write = async {
        let mut tx = pool.begin().await?;
        let reason = guards::authorize_write(&mut tx, &ctx).await?;
        if reason.is_none() {
            sqlx::query("INSERT INTO test_effects(id) VALUES($1)")
                .bind(&ctx.idempotency_key)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok::<_, KatError>(reason)
    };
    let release = async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id FROM kats%')")
                .fetch_one(&pool).await.unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
        loop {
            let expired: bool = sqlx::query_scalar(
                "SELECT expires_at <= clock_timestamp() FROM task_leases WHERE task_id=$1",
            )
            .bind(h.input.task_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            if expired {
                break;
            }
            tokio::task::yield_now().await;
        }
        blocker.rollback().await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(write, release)
    })
    .await
    .expect("write authorization reached the Kat lock before lease expiry");
    assert_eq!(
        count_effects(&pool).await,
        0,
        "expired lease authorized a committed effect: {result:?}"
    );
    assert!(matches!(result, Err(KatError::LeaseLost)));
}
