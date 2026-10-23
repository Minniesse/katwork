use super::{ToolCallState, ToolCtx, guards};
use crate::{
    db,
    error::KatError,
    fence::{self, Fence},
    ids::*,
    types::SideEffect,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

pub(crate) fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub(crate) fn clean(text: &str) -> String {
    text.replace('\0', "\u{fffd}")
}
pub(crate) async fn transaction(pool: &PgPool) -> Result<Transaction<'_, Postgres>, KatError> {
    let mut tx = pool.begin().await?;
    db::set_configured_lock_timeout(&mut tx).await?;
    Ok(tx)
}
async fn insert(
    ctx: &ToolCtx,
    name: &str,
    effect: Option<SideEffect>,
    args: &str,
    result: Option<&str>,
) -> Result<ToolCallRecordId, KatError> {
    let mut tx = transaction(&ctx.pool).await?;
    guards::scoped_task(&mut tx, &ctx.tenant, &ctx.fence()).await?;
    let starts_effect = result.is_none() && effect != Some(SideEffect::Read);
    if starts_effect {
        fence::lock(&mut tx, &ctx.fence()).await?;
    } else {
        fence::check(&mut tx, &ctx.fence()).await?;
    }
    let id = ToolCallRecordId::new();
    let inserted=sqlx::query("INSERT INTO tool_calls(id,workspace_id,run_id,step,tool_call_id,tool_name,side_effect,args_canonical,args_sha256,state,result,ended_at)
        SELECT $1,$2,r.id,$4,$5,$6,$7,$8,$9,$10,$11,CASE WHEN $11::text IS NOT NULL THEN clock_timestamp() END
        FROM runs r WHERE r.id=$3 AND r.workspace_id=$2 AND r.task_id=$12 AND r.kat_id=$13 AND r.job_id=$14 AND r.state='running' AND r.lease_epoch=$15")
        .bind(id).bind(ctx.tenant.workspace_id()).bind(ctx.run_id).bind(i32::try_from(ctx.step).map_err(|_|KatError::RequestInvalid)?)
        .bind(clean(&ctx.tool_call_id)).bind(clean(name)).bind(effect.map(|e|e.wire())).bind(clean(args)).bind(sha256(args))
        .bind(if result.is_some(){"refused"}else{"started"}).bind(result.map(clean)).bind(ctx.task_id).bind(ctx.kat_id).bind(ctx.job_id).bind(ctx.lease_epoch)
        .execute(&mut *tx).await?;
    if inserted.rows_affected() != 1 {
        return Err(KatError::LeaseLost);
    }
    if starts_effect {
        sqlx::query("UPDATE tasks SET effects_started_at=COALESCE(effects_started_at,clock_timestamp()) WHERE workspace_id=$1 AND id=$2")
            .bind(ctx.tenant.workspace_id()).bind(ctx.task_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}
pub async fn start_tool_call(
    pool: &PgPool,
    ctx: &ToolCtx,
    name: &str,
    side_effect: SideEffect,
    args_canonical: &str,
) -> Result<ToolCallRecordId, KatError> {
    let ctx = ToolCtx {
        pool: pool.clone(),
        ..ctx.clone()
    };
    insert(&ctx, name, Some(side_effect), args_canonical, None).await
}
pub async fn record_refused(
    pool: &PgPool,
    ctx: &ToolCtx,
    name: &str,
    side_effect: Option<SideEffect>,
    args_canonical: &str,
    code: &str,
) -> Result<ToolCallRecordId, KatError> {
    let ctx = ToolCtx {
        pool: pool.clone(),
        ..ctx.clone()
    };
    insert(&ctx, name, side_effect, args_canonical, Some(code)).await
}
pub async fn retry_read(
    pool: &PgPool,
    fence: &Fence,
    id: ToolCallRecordId,
) -> Result<(), KatError> {
    let mut tx = transaction(pool).await?;
    fence::check(&mut tx, fence).await?;
    let changed=sqlx::query("UPDATE tool_calls c SET attempts=2,state='started',ended_at=NULL,result=NULL FROM runs r,tasks t
        WHERE c.id=$1 AND c.run_id=r.id AND c.workspace_id=r.workspace_id AND r.task_id=t.id AND r.workspace_id=t.workspace_id
        AND t.id=$2 AND r.lease_epoch=$3 AND r.state='running' AND c.side_effect='read' AND c.attempts=1 AND c.state IN ('timed_out','outcome_unknown')")
        .bind(id).bind(fence.task_id).bind(fence.epoch).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(KatError::RequestInvalid);
    }
    tx.commit().await?;
    Ok(())
}
pub async fn end_tool_call(
    pool: &PgPool,
    fence: &Fence,
    id: ToolCallRecordId,
    state: ToolCallState,
    result: Option<&str>,
) -> Result<(), KatError> {
    if matches!(state, ToolCallState::Started | ToolCallState::Refused) {
        return Err(KatError::RequestInvalid);
    }
    let mut tx = transaction(pool).await?;
    fence::check(&mut tx, fence).await?;
    let changed=sqlx::query("UPDATE tool_calls c SET state=$4,result=$5,ended_at=clock_timestamp() FROM runs r,tasks t
        WHERE c.id=$1 AND c.run_id=r.id AND c.workspace_id=r.workspace_id AND r.task_id=t.id AND r.workspace_id=t.workspace_id
        AND t.id=$2 AND r.lease_epoch=$3 AND r.state='running' AND c.state='started'")
        .bind(id).bind(fence.task_id).bind(fence.epoch).bind(state.wire()).bind(result.map(clean)).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(KatError::LeaseLost);
    }
    tx.commit().await?;
    Ok(())
}
// Only the owner of an unpolled call may undo its conservative effects marker.
pub(crate) async fn refuse_started(
    pool: &PgPool,
    fence: &Fence,
    id: ToolCallRecordId,
    code: &str,
) -> Result<(), KatError> {
    let mut tx = transaction(pool).await?;
    fence::lock(&mut tx, fence).await?;
    let changed = sqlx::query(
        "UPDATE tool_calls c SET state='refused',result=$4,ended_at=clock_timestamp() FROM runs r
        WHERE c.id=$1 AND c.run_id=r.id AND c.workspace_id=r.workspace_id AND r.task_id=$2
        AND r.lease_epoch=$3 AND r.state='running' AND c.state='started'",
    )
    .bind(id)
    .bind(fence.task_id)
    .bind(fence.epoch)
    .bind(clean(code))
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(KatError::LeaseLost);
    }
    sqlx::query("UPDATE tasks t SET effects_started_at=NULL WHERE t.id=$1
        AND NOT EXISTS (SELECT 1 FROM tool_calls c JOIN runs r ON r.id=c.run_id AND r.workspace_id=c.workspace_id
            WHERE r.task_id=t.id AND r.workspace_id=t.workspace_id AND c.side_effect <> 'read' AND c.state <> 'refused')")
        .bind(fence.task_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
#[derive(Debug, Clone)]
pub struct ModelCallRow {
    pub id: ModelCallId,
    pub workspace_id: WorkspaceId,
    pub space_id: SpaceId,
    pub job_id: JobId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub kat_id: KatId,
    pub provider_id: String,
    pub model: String,
    pub residency: String,
    pub attempt: i32,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub usage_complete: bool,
    pub charged_tokens: i64,
    pub cost_micros: Option<i64>,
    pub latency_ms: i32,
    pub outcome: String,
}
pub async fn record_model_call(
    pool: &PgPool,
    fence: &Fence,
    row: &ModelCallRow,
) -> Result<ModelCallId, KatError> {
    let mut tx = transaction(pool).await?;
    sqlx::query("SELECT id FROM tasks WHERE workspace_id=$1 AND id=$2")
        .bind(row.workspace_id)
        .bind(fence.task_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KatError::NotFound)?;
    fence::check(&mut tx, fence).await?;
    if row.task_id != fence.task_id {
        return Err(KatError::RequestInvalid);
    }
    let inserted=sqlx::query("INSERT INTO model_calls(id,workspace_id,space_id,job_id,task_id,run_id,kat_id,provider_id,model,residency,attempt,input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,usage_complete,charged_tokens,cost_micros,latency_ms,outcome)
        SELECT $1,$2,$3,$4,$5,r.id,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20 FROM runs r
        WHERE r.workspace_id=$2 AND r.id=$6 AND r.task_id=$5 AND r.job_id=$4 AND r.kat_id=$7 AND r.lease_epoch=$21 AND r.state='running'")
        .bind(row.id).bind(row.workspace_id).bind(row.space_id).bind(row.job_id).bind(row.task_id).bind(row.run_id).bind(row.kat_id)
        .bind(clean(&row.provider_id)).bind(clean(&row.model)).bind(&row.residency).bind(row.attempt)
        .bind(row.input_tokens).bind(row.cache_read_tokens).bind(row.cache_write_tokens).bind(row.output_tokens).bind(row.usage_complete)
        .bind(row.charged_tokens).bind(row.cost_micros).bind(row.latency_ms).bind(&row.outcome).bind(fence.epoch).execute(&mut *tx).await?;
    if inserted.rows_affected() != 1 {
        return Err(KatError::LeaseLost);
    }
    tx.commit().await?;
    Ok(row.id)
}
