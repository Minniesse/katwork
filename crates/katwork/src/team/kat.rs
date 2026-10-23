use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgConnection, PgPool, Postgres};
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::{
    error::KatError,
    ids::{KatId, SpaceId},
    tenancy::{self, Tenant},
    types::{KatStatus, Locale, ModelTier},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KatRole {
    Lead,
    Researcher,
    Writer,
    Analyst,
    Scheduler,
    Reviewer,
    Custom(String),
}

impl KatRole {
    fn stored(&self) -> String {
        match self {
            Self::Lead => "lead".into(),
            Self::Researcher => "researcher".into(),
            Self::Writer => "writer".into(),
            Self::Analyst => "analyst".into(),
            Self::Scheduler => "scheduler".into(),
            Self::Reviewer => "reviewer".into(),
            Self::Custom(name) => format!("custom:{name}"),
        }
    }
}

impl TryFrom<String> for KatRole {
    type Error = sqlx::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Ok(match value.as_str() {
            "lead" => Self::Lead,
            "researcher" => Self::Researcher,
            "writer" => Self::Writer,
            "analyst" => Self::Analyst,
            "scheduler" => Self::Scheduler,
            "reviewer" => Self::Reviewer,
            _ => Self::Custom(value.strip_prefix("custom:").unwrap_or(&value).to_owned()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Autonomy {
    AskFirst,
    Standard,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub avatar: String,
    pub accent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Kat {
    pub id: KatId,
    pub default_space_id: SpaceId,
    pub spaces: Vec<SpaceId>,
    pub name: String,
    #[sqlx(try_from = "String")]
    pub role: KatRole,
    pub persona: String,
    pub instructions: String,
    pub tool_allowlist: Vec<String>,
    pub model_tier: ModelTier,
    pub autonomy: Autonomy,
    pub status: KatStatus,
    #[sqlx(json)]
    pub appearance: Appearance,
    #[sqlx(try_from = "i32")]
    pub max_concurrency: u16,
    pub daily_token_budget: Option<i64>,
    pub policy_version: i64,
}

impl Kat {
    pub fn config_hash(&self) -> String {
        let mut config = serde_json::json!({
            "persona": self.persona, "instructions": self.instructions,
            "tool_allowlist": self.tool_allowlist, "model_tier": self.model_tier,
            "autonomy": self.autonomy, "policy_version": self.policy_version,
        });
        // Dependency features can enable insertion order; audit hashes require sorted keys.
        config.sort_all_objects();
        format!("{:x}", Sha256::digest(config.to_string().as_bytes()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewKat {
    pub name: String,
    pub role: KatRole,
    pub persona: String,
    pub instructions: String,
    pub tool_allowlist: Vec<String>,
    pub model_tier: ModelTier,
    pub autonomy: Autonomy,
    pub appearance: Appearance,
    pub max_concurrency: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HireKat {
    pub name: String,
    pub role: KatRole,
    pub persona: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KatPatch {
    pub name: Option<String>,
    pub persona: Option<String>,
    pub instructions: Option<String>,
    pub tool_allowlist: Option<Vec<String>>,
    pub model_tier: Option<ModelTier>,
    pub autonomy: Option<Autonomy>,
    pub status: Option<KatStatus>,
    pub appearance: Option<Appearance>,
    pub max_concurrency: Option<u16>,
    #[serde(default, deserialize_with = "double_option")]
    pub daily_token_budget: Option<Option<i64>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<i64>>, D::Error> {
    Option::deserialize(deserializer).map(Some)
}

fn validate_config(
    persona: &str,
    instructions: &str,
    appearance: &Appearance,
    tools: &mut Vec<String>,
) -> Result<(), KatError> {
    for (text, limit, multiline) in [
        (persona, 4_000, true),
        (instructions, 8_000, true),
        (appearance.avatar.as_str(), 200, false),
        (appearance.accent.as_str(), 200, false),
    ] {
        if text.chars().count() > limit
            || text.chars().any(|ch| {
                matches!(
                    ch.general_category(),
                    GeneralCategory::Control
                        | GeneralCategory::Format
                        | GeneralCategory::LineSeparator
                        | GeneralCategory::ParagraphSeparator
                ) && !(multiline && matches!(ch, '\n' | '\r' | '\t'))
            })
        {
            return Err(KatError::RequestInvalid);
        }
    }
    if !matches!(
        appearance.avatar.as_str(),
        "compass" | "magnifier" | "pen" | "chart" | "calendar" | "check" | "kat"
    ) || !matches!(
        appearance.accent.as_str(),
        "blue" | "yellow" | "red" | "green" | "purple"
    ) {
        return Err(KatError::RequestInvalid);
    }
    if tools.iter().any(|id| {
        id.len() > 64
            || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !id.bytes().all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'_' | b'.')
            })
    }) {
        return Err(KatError::RequestInvalid);
    }
    tools.sort();
    tools.dedup();
    if tools.len() > 32 {
        return Err(KatError::RequestInvalid);
    }
    Ok(())
}

pub async fn get_on<'e, E: Executor<'e, Database = Postgres>>(
    executor: E,
    tenant: &Tenant,
    id: KatId,
) -> Result<Kat, KatError> {
    sqlx::query_as("SELECT d.*, ARRAY(SELECT s.space_id FROM kat_spaces s WHERE s.kat_id = d.id AND s.workspace_id = d.workspace_id ORDER BY s.space_id) AS spaces FROM kats d WHERE d.workspace_id = $1 AND d.id = $2")
        .bind(tenant.workspace_id())
        .bind(id)
        .fetch_optional(executor)
        .await?
        .ok_or(KatError::NotFound)
}

pub async fn get(pool: &PgPool, tenant: &Tenant, id: KatId) -> Result<Kat, KatError> {
    get_on(pool, tenant, id).await
}

/// Check the grant independently of status so callers can distinguish a paused Kat from a revoked grant.
pub async fn is_granted(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: KatId,
    space: SpaceId,
) -> Result<bool, KatError> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kat_spaces WHERE workspace_id=$1 AND kat_id=$2 AND space_id=$3)")
        .bind(tenant.workspace_id()).bind(id).bind(space).fetch_one(conn).await?)
}

/// Check both active status and the grant in one snapshot on the caller's connection.
pub async fn is_eligible(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: KatId,
    space: SpaceId,
) -> Result<bool, KatError> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kats d JOIN kat_spaces s ON s.kat_id=d.id AND s.workspace_id=d.workspace_id WHERE d.workspace_id=$1 AND d.id=$2 AND d.status='active' AND s.space_id=$3)")
        .bind(tenant.workspace_id()).bind(id).bind(space).fetch_one(conn).await?)
}

pub async fn list(pool: &PgPool, tenant: &Tenant, space: SpaceId) -> Result<Vec<Kat>, KatError> {
    tenancy::space::get(pool, tenant, space).await?;
    Ok(sqlx::query_as("SELECT d.*, ARRAY(SELECT s.space_id FROM kat_spaces s WHERE s.kat_id = d.id AND s.workspace_id = d.workspace_id ORDER BY s.space_id) AS spaces FROM kats d WHERE d.workspace_id = $1 AND EXISTS (SELECT 1 FROM kat_spaces s WHERE s.kat_id = d.id AND s.workspace_id = $1 AND s.space_id = $2) ORDER BY d.id")
        .bind(tenant.workspace_id()).bind(space).fetch_all(pool).await?)
}

pub async fn hire(
    pool: &PgPool,
    tenant: &Tenant,
    space: SpaceId,
    new: NewKat,
) -> Result<KatId, KatError> {
    let mut tx = pool.begin().await?;
    super::lock_workspace(&mut tx, tenant).await?;
    let id = hire_on(&mut tx, tenant, space, new).await?;
    tx.commit().await?;
    Ok(id)
}

pub(super) async fn hire_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space: SpaceId,
    mut new: NewKat,
) -> Result<KatId, KatError> {
    super::check_space(conn, tenant, space).await?;
    new.name = super::validate_name(&new.name)?.to_owned();
    if let KatRole::Custom(name) = &mut new.role {
        *name = tenancy::validate_name(name)?.to_owned();
    }
    validate_config(
        &new.persona,
        &new.instructions,
        &new.appearance,
        &mut new.tool_allowlist,
    )?;
    if new.max_concurrency == 0 {
        return Err(KatError::RequestInvalid);
    }
    // A template in either language is a default hire; anything else is an execution setting.
    let is_template = [Locale::En, Locale::Th].into_iter().any(|locale| {
        let template = super::roles::template(&new.role, locale);
        new.instructions == template.instructions
            && new.tool_allowlist == template.tool_allowlist
            && new.model_tier == template.model_tier
            && new.autonomy == template.autonomy
            && new.max_concurrency == template.max_concurrency
    });
    if !is_template {
        tenant.require_admin()?;
    }
    let id = KatId::new();
    sqlx::query("INSERT INTO kats (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy, appearance, max_concurrency) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(id).bind(tenant.workspace_id()).bind(space).bind(new.name).bind(new.role.stored())
        .bind(new.persona).bind(new.instructions).bind(new.tool_allowlist).bind(new.model_tier).bind(new.autonomy)
        .bind(sqlx::types::Json(new.appearance)).bind(i32::from(new.max_concurrency)).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO kat_spaces (workspace_id, kat_id, space_id) VALUES ($1,$2,$3)")
        .bind(tenant.workspace_id())
        .bind(id)
        .bind(space)
        .execute(conn)
        .await?;
    Ok(id)
}

async fn lock_kat(conn: &mut PgConnection, tenant: &Tenant, id: KatId) -> Result<(), KatError> {
    sqlx::query("SELECT id FROM kats WHERE workspace_id = $1 AND id = $2 FOR NO KEY UPDATE")
        .bind(tenant.workspace_id())
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(KatError::NotFound)?;
    Ok(())
}

pub async fn update(
    pool: &PgPool,
    tenant: &Tenant,
    id: KatId,
    patch: KatPatch,
) -> Result<Kat, KatError> {
    if patch.instructions.is_some()
        || patch.tool_allowlist.is_some()
        || patch.model_tier.is_some()
        || patch.autonomy.is_some()
        || patch.status.is_some()
        || patch.max_concurrency.is_some()
        || patch.daily_token_budget.is_some()
    {
        tenant.require_admin()?;
    }
    let mut tx = pool.begin().await?;
    super::lock_workspace(&mut tx, tenant).await?;
    lock_kat(&mut tx, tenant, id).await?;
    let mut kat = get_on(&mut *tx, tenant, id).await?;
    if let Some(name) = patch.name {
        let name = super::validate_name(&name)?;
        let others: Vec<String> = sqlx::query_scalar("SELECT d.name FROM team_members mine JOIN team_members peer ON peer.team_id = mine.team_id AND peer.workspace_id = mine.workspace_id JOIN kats d ON d.id = peer.kat_id AND d.workspace_id = peer.workspace_id WHERE mine.workspace_id = $1 AND mine.kat_id = $2 AND mine.active AND peer.active AND peer.kat_id <> $2")
            .bind(tenant.workspace_id()).bind(id).fetch_all(&mut *tx).await?;
        if others
            .iter()
            .any(|other| super::name_key(other) == super::name_key(name))
        {
            return Err(KatError::KatNameConflict);
        }
        kat.name = name.to_owned();
    }
    kat.tool_allowlist.sort();
    kat.tool_allowlist.dedup();
    let before = (
        kat.tool_allowlist.clone(),
        kat.status,
        kat.autonomy,
        kat.model_tier,
    );
    if let Some(value) = patch.persona {
        kat.persona = value;
    }
    if let Some(value) = patch.instructions {
        kat.instructions = value;
    }
    if let Some(value) = patch.tool_allowlist {
        kat.tool_allowlist = value;
    }
    if let Some(value) = patch.model_tier {
        kat.model_tier = value;
    }
    if let Some(value) = patch.autonomy {
        kat.autonomy = value;
    }
    if let Some(value) = patch.status {
        kat.status = value;
    }
    if let Some(value) = patch.appearance {
        kat.appearance = value;
    }
    if let Some(value) = patch.max_concurrency {
        kat.max_concurrency = value;
    }
    if let Some(value) = patch.daily_token_budget {
        kat.daily_token_budget = value;
    }
    if kat.max_concurrency == 0 || kat.daily_token_budget.is_some_and(|n| n < 0) {
        return Err(KatError::RequestInvalid);
    }
    validate_config(
        &kat.persona,
        &kat.instructions,
        &kat.appearance,
        &mut kat.tool_allowlist,
    )?;
    if before
        != (
            kat.tool_allowlist.clone(),
            kat.status,
            kat.autonomy,
            kat.model_tier,
        )
    {
        kat.policy_version += 1;
    }
    sqlx::query("UPDATE kats SET name=$3, persona=$4, instructions=$5, tool_allowlist=$6, model_tier=$7, autonomy=$8, status=$9, appearance=$10, max_concurrency=$11, daily_token_budget=$12, policy_version=$13 WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id()).bind(id).bind(&kat.name).bind(&kat.persona).bind(&kat.instructions)
        .bind(&kat.tool_allowlist).bind(kat.model_tier).bind(kat.autonomy).bind(kat.status)
        .bind(sqlx::types::Json(&kat.appearance)).bind(i32::from(kat.max_concurrency)).bind(kat.daily_token_budget)
        .bind(kat.policy_version).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(kat)
}

pub async fn grant_space(
    pool: &PgPool,
    tenant: &Tenant,
    id: KatId,
    space: SpaceId,
) -> Result<(), KatError> {
    change_grant(pool, tenant, id, space, true).await
}

pub async fn revoke_space(
    pool: &PgPool,
    tenant: &Tenant,
    id: KatId,
    space: SpaceId,
) -> Result<(), KatError> {
    change_grant(pool, tenant, id, space, false).await
}

async fn change_grant(
    pool: &PgPool,
    tenant: &Tenant,
    id: KatId,
    space: SpaceId,
    grant: bool,
) -> Result<(), KatError> {
    tenant.require_admin()?;
    let mut tx = pool.begin().await?;
    super::lock_workspace(&mut tx, tenant).await?;
    lock_kat(&mut tx, tenant, id).await?;
    super::check_space(&mut tx, tenant, space).await?;
    if !grant && get_on(&mut *tx, tenant, id).await?.default_space_id == space {
        return Err(KatError::DefaultSpaceRequired);
    }
    if !grant {
        let in_use: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM team_members m JOIN teams t ON t.id=m.team_id AND t.workspace_id=m.workspace_id WHERE m.workspace_id=$1 AND m.kat_id=$2 AND m.active AND t.space_id=$3)")
            .bind(tenant.workspace_id()).bind(id).bind(space).fetch_one(&mut *tx).await?;
        if in_use {
            return Err(KatError::GrantInUse);
        }
    }
    let mutation = if grant {
        "WITH changed AS (INSERT INTO kat_spaces (workspace_id, kat_id, space_id) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING RETURNING kat_id) UPDATE kats SET policy_version=policy_version+1 WHERE workspace_id=$1 AND id=$2 AND EXISTS (SELECT 1 FROM changed)"
    } else {
        "WITH changed AS (DELETE FROM kat_spaces WHERE workspace_id=$1 AND kat_id=$2 AND space_id=$3 RETURNING kat_id) UPDATE kats SET policy_version=policy_version+1 WHERE workspace_id=$1 AND id=$2 AND EXISTS (SELECT 1 FROM changed)"
    };
    sqlx::query(mutation)
        .bind(tenant.workspace_id())
        .bind(id)
        .bind(space)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
