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
/// Every path ending a lease must change `tasks.state` or `tasks.lease_epoch` in that transaction:
/// after a lock wait, Postgres re-checks only the locked task row, not the joined lease row.
/// Expiry comparisons use `clock_timestamp()` because transaction time may predate a lock wait.
pub async fn check(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError> {
    check_with_lock(
        conn,
        f,
        "SELECT tasks.id FROM tasks JOIN task_leases ON task_leases.task_id = tasks.id
         WHERE tasks.id = $1 AND tasks.state = 'leased'
           AND tasks.lease_epoch = $2 AND task_leases.lease_epoch = $2
           AND task_leases.expires_at > clock_timestamp() FOR SHARE OF tasks",
    )
    .await
}

/// Use this instead of `check` when the transaction will update the task; never upgrade a share lock.
/// Every path ending a lease must change `tasks.state` or `tasks.lease_epoch` in that transaction:
/// after a lock wait, Postgres re-checks only the locked task row, not the joined lease row.
/// Expiry comparisons use `clock_timestamp()` because transaction time may predate a lock wait.
pub async fn lock(conn: &mut PgConnection, f: &Fence) -> Result<(), KatError> {
    check_with_lock(
        conn,
        f,
        "SELECT tasks.id FROM tasks JOIN task_leases ON task_leases.task_id = tasks.id
         WHERE tasks.id = $1 AND tasks.state = 'leased'
           AND tasks.lease_epoch = $2 AND task_leases.lease_epoch = $2
           AND task_leases.expires_at > clock_timestamp() FOR NO KEY UPDATE OF tasks",
    )
    .await
}

async fn check_with_lock(
    conn: &mut PgConnection,
    f: &Fence,
    query: &'static str,
) -> Result<(), KatError> {
    let task = sqlx::query_scalar::<_, TaskId>(query)
        .bind(f.task_id)
        .bind(f.epoch)
        .fetch_optional(conn)
        .await?;
    task.map(|_| ()).ok_or(KatError::LeaseLost)
}
