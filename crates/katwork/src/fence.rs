use sqlx::PgConnection;

use crate::{
    error::KatError,
    ids::{LeaseEpoch, TaskId},
};

#[derive(Debug, Clone, Copy)]
pub struct Fence {
    pub task_id: TaskId,
    pub epoch: LeaseEpoch,
}

/// Call inside the write transaction, after any workspace/job locks and before later row locks.
/// The shared task lock remains held until that transaction ends.
/// Lock the task first, then check state, both epochs and expiry in a separate statement.
/// Expiry uses `clock_timestamp()` after any lock wait, even if the holder changed no rows.
/// Every path ending a lease must change `tasks.state` or `tasks.lease_epoch` in that transaction:
/// its task lock serializes the transition with fenced writes.
pub async fn check(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError> {
    check_with_lock(conn, f, "SELECT id FROM tasks WHERE id = $1 FOR SHARE").await
}

/// Use this instead of `check` when the transaction will update the task; never upgrade a share lock.
/// The same transaction, lock order and lease checks as `check` apply, using `FOR NO KEY UPDATE`.
pub async fn lock(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError> {
    check_with_lock(
        conn,
        f,
        "SELECT id FROM tasks WHERE id = $1 FOR NO KEY UPDATE",
    )
    .await
}

async fn check_with_lock(
    conn: &mut PgConnection,
    f: &Fence,
    query: &'static str,
) -> Result<(), KatError> {
    sqlx::query(query)
        .bind(f.task_id)
        .execute(&mut *conn)
        .await?;
    let task = sqlx::query_scalar::<_, TaskId>(
        "SELECT tasks.id FROM tasks JOIN task_leases ON task_leases.task_id = tasks.id
         WHERE tasks.id = $1 AND tasks.state = 'leased'
           AND tasks.lease_epoch = $2 AND task_leases.lease_epoch = $2
           AND task_leases.expires_at > clock_timestamp()",
    )
    .bind(f.task_id)
    .bind(f.epoch)
    .fetch_optional(conn)
    .await?;
    task.map(|_| ()).ok_or(KatError::LeaseLost)
}
