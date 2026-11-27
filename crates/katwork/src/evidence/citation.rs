//! Citations: what `cite` stores, one row per job, span and quoted range. Writers (Task 9) load
//! the citations a Kat's claims name, lock their source artifacts and copy their refs.
use sqlx::{PgConnection, types::Json};

use super::source_ref::SourceRef;
use crate::{
    error::KatError,
    ids::{ArtifactId, CitationId, JobId, SpanId},
    tenancy::Tenant,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CitationRow {
    pub id: CitationId,
    pub span_id: SpanId,
    /// The cited span's artifact: the writers lock its row (the artifact lock contract).
    pub artifact_id: ArtifactId,
    pub job_id: JobId,
    pub exact: String,
    pub source_ref: SourceRef,
    pub display: String,
}

/// The citations of `job_id` among `ids`, in ascending id. An id of another job or workspace,
/// or one that does not exist, is missing from the result; the caller compares lengths.
pub async fn get_for_job_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    job_id: JobId,
    ids: &[CitationId],
) -> Result<Vec<CitationRow>, KatError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT c.id, c.span_id, v.artifact_id, c.job_id, c.exact, c.source_ref, c.display
         FROM citations c
         JOIN source_spans s ON s.id = c.span_id AND s.workspace_id = c.workspace_id
         JOIN artifact_versions v
           ON v.id = s.artifact_version_id AND v.workspace_id = s.workspace_id
         WHERE c.workspace_id = $1 AND c.job_id = $2 AND c.id = ANY($3)
         ORDER BY c.id",
    )
    .bind(tenant.workspace_id())
    .bind(job_id)
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(Row::into_citation).collect())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: CitationId,
    span_id: SpanId,
    artifact_id: ArtifactId,
    job_id: JobId,
    exact: String,
    source_ref: Json<SourceRef>,
    display: String,
}

impl Row {
    fn into_citation(self) -> CitationRow {
        CitationRow {
            id: self.id,
            span_id: self.span_id,
            artifact_id: self.artifact_id,
            job_id: self.job_id,
            exact: self.exact,
            source_ref: self.source_ref.0,
            display: self.display,
        }
    }
}
