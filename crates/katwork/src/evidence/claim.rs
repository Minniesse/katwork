//! Claim types shared by the writers (Task 9) and the verifier (Tasks 10 and 11), and the claim
//! records a writer stores with its output version.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use super::citation::CitationRow;
use crate::{
    error::KatError,
    ids::{ArtifactVersionId, CitationId, ClaimId},
    tenancy::Tenant,
};

/// The longest claim text, in code points (a claim is one sentence or one figure with its words).
pub const MAX_CLAIM_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Number,
    Date,
    Quote,
    Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveOp {
    Sum,
    Difference,
    Ratio,
    Share,
    PercentChange,
}

/// A figure computed from cited values: `args` index the claim's `cite` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Derive {
    pub op: DeriveOp,
    pub args: Vec<u8>,
}

impl Derive {
    /// `Sum` takes two or more arguments, every other operation exactly two.
    pub fn arity_ok(&self) -> bool {
        match self.op {
            DeriveOp::Sum => self.args.len() >= 2,
            _ => self.args.len() == 2,
        }
    }

    /// The arity is right, every argument indexes a `cite` list of `cite_len` entries, and no
    /// argument is named twice (one figure counted twice would let any multiple of it pass).
    pub fn indices_in(&self, cite_len: usize) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        self.arity_ok()
            && self
                .args
                .iter()
                .all(|&i| usize::from(i) < cite_len && seen.insert(i))
    }
}

/// A claim a writer records at `location` (`p:3`, `Sheet1!B7`, `slide:2:shape:5`) of the
/// output version it wrote. `cite` lists the Kat's citation ids, in its order; `derive.args`
/// index that list. `cited` is true exactly when `cite` is not empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClaim {
    pub ordinal: u32,
    pub kind: ClaimKind,
    pub text: String,
    pub location: String,
    pub cited: bool,
    pub derive: Option<Derive>,
    pub cite: Vec<CitationId>,
}

/// Inserts `claims` for `version`, each with a `claim_refs` row per cite id that copies its
/// citation's `source_ref` byte for byte, inside the caller's transaction (Task 9 calls it after
/// `store::lock_artifacts_on`). `citations` are the rows `citation::get_for_job_on` returned
/// for the Kat's job: a cite id not among them is `CitationUnknown`. A claim with empty or
/// over-long text, an empty location, a `cited` flag that disagrees with `cite`, a `derive`
/// whose arguments do not index `cite`, or an ordinal used twice is `RequestInvalid`. Every
/// claim is checked before anything is inserted; on an error the caller must not commit.
pub async fn insert_claims_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    version: ArtifactVersionId,
    claims: &[NewClaim],
    citations: &[CitationRow],
) -> Result<(), KatError> {
    if claims.is_empty() {
        return Ok(());
    }
    let known: HashSet<CitationId> = citations.iter().map(|c| c.id).collect();
    let mut ordinals_seen = HashSet::with_capacity(claims.len());
    let n = claims.len();
    let (mut ids, mut ordinals, mut kinds, mut texts) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut locations, mut cited, mut derives) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut ref_claims, mut ref_ordinals, mut ref_citations) = (vec![], vec![], vec![]);
    for claim in claims {
        let text = claim.text.replace('\0', "\u{FFFD}");
        let location = claim.location.replace('\0', "\u{FFFD}");
        let chars = text.chars().count();
        if chars == 0
            || chars > MAX_CLAIM_CHARS
            || location.is_empty()
            || claim.cited == claim.cite.is_empty()
            || claim
                .derive
                .as_ref()
                .is_some_and(|d| !d.indices_in(claim.cite.len()))
            || !ordinals_seen.insert(claim.ordinal)
        {
            return Err(KatError::RequestInvalid);
        }
        if !claim.cite.iter().all(|id| known.contains(id)) {
            return Err(KatError::CitationUnknown);
        }
        let id = ClaimId::new();
        for (ordinal, citation) in claim.cite.iter().enumerate() {
            ref_claims.push(id);
            ref_ordinals.push(i32::try_from(ordinal).map_err(|_| KatError::RequestInvalid)?);
            ref_citations.push(*citation);
        }
        ids.push(id);
        ordinals.push(i32::try_from(claim.ordinal).map_err(|_| KatError::RequestInvalid)?);
        kinds.push(kind_wire(claim.kind));
        texts.push(text);
        locations.push(location);
        cited.push(claim.cited);
        // `to_string`, never a `Value`, as every JSONB this crate binds.
        derives.push(
            claim
                .derive
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| KatError::RequestInvalid)?,
        );
    }
    let workspace = tenant.workspace_id();
    sqlx::query(
        "INSERT INTO claims (id, workspace_id, artifact_version_id, ordinal, kind, text, location,
             cited, derive)
         SELECT u.id, $2, $3, u.ordinal, u.kind, u.text, u.location, u.cited, u.derive::jsonb
         FROM UNNEST($1::uuid[], $4::int[], $5::text[], $6::text[], $7::text[], $8::bool[],
                     $9::text[])
              AS u(id, ordinal, kind, text, location, cited, derive)",
    )
    .bind(&ids)
    .bind(workspace)
    .bind(version)
    .bind(&ordinals)
    .bind(&kinds)
    .bind(&texts)
    .bind(&locations)
    .bind(&cited)
    .bind(&derives)
    .execute(&mut *conn)
    .await?;
    if ref_claims.is_empty() {
        return Ok(());
    }
    // The ref is copied in SQL: a round trip through `SourceRef` or `serde_json::Value` could
    // change it (a widened `f32`, a redacted cell dropped).
    let copied = sqlx::query(
        "INSERT INTO claim_refs (claim_id, workspace_id, ordinal, citation_id, source_ref)
         SELECT u.claim_id, $1, u.ordinal, c.id, c.source_ref
         FROM UNNEST($2::uuid[], $3::int[], $4::uuid[]) AS u(claim_id, ordinal, citation_id)
         JOIN citations c
           ON c.id = u.citation_id AND c.workspace_id = $1 AND c.id = ANY($5)",
    )
    .bind(workspace)
    .bind(&ref_claims)
    .bind(&ref_ordinals)
    .bind(&ref_citations)
    // Only the citations the writer loaded for its job: the row count then refuses any other.
    .bind(citations.iter().map(|c| c.id).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if copied != ref_claims.len() as u64 {
        return Err(KatError::CitationUnknown);
    }
    Ok(())
}

fn kind_wire(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Number => "number",
        ClaimKind::Date => "date",
        ClaimKind::Quote => "quote",
        ClaimKind::Fact => "fact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claim_types_serde_wire_names() {
        let kinds = [
            (ClaimKind::Number, "number"),
            (ClaimKind::Date, "date"),
            (ClaimKind::Quote, "quote"),
            (ClaimKind::Fact, "fact"),
        ];
        for (kind, wire) in kinds {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        }
        let ops = [
            (DeriveOp::Sum, "sum"),
            (DeriveOp::Difference, "difference"),
            (DeriveOp::Ratio, "ratio"),
            (DeriveOp::Share, "share"),
            (DeriveOp::PercentChange, "percent_change"),
        ];
        for (op, wire) in ops {
            assert_eq!(serde_json::to_value(op).unwrap(), json!(wire));
        }
        let derive: Derive = serde_json::from_value(json!({"op":"sum","args":[0,1]})).unwrap();
        assert_eq!(derive.args, vec![0, 1]);
        assert!(serde_json::from_value::<Derive>(json!({"op":"sum","args":[300]})).is_err());
        assert!(serde_json::from_value::<Derive>(json!({"op":"sum","args":[0,1],"x":1})).is_err());
    }

    #[test]
    fn derive_arity() {
        let d = |op, args: &[u8]| Derive {
            op,
            args: args.to_vec(),
        };
        assert!(d(DeriveOp::Sum, &[0, 1, 2]).arity_ok());
        assert!(!d(DeriveOp::Sum, &[0]).arity_ok());
        assert!(d(DeriveOp::Ratio, &[0, 1]).arity_ok());
        assert!(!d(DeriveOp::Ratio, &[0, 1, 2]).arity_ok());
        assert!(d(DeriveOp::Share, &[0, 1]).indices_in(2));
        assert!(!d(DeriveOp::Share, &[0, 2]).indices_in(2));
    }
}
