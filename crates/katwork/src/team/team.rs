use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::{Connection, Executor, FromRow, PgConnection, PgPool, Postgres, Row};

use super::{Kat, kat};
use crate::{
    error::KatError,
    ids::{KatId, SpaceId, TeamId},
    tenancy::{self, Tenant},
    types::Locale,
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Team {
    pub id: TeamId,
    pub space_id: SpaceId,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct TeamView {
    pub team: Team,
    pub members: Vec<Kat>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTeam {
    pub name: String,
    pub lead: KatId,
    pub members: Vec<KatId>,
}

async fn get_team<'e, E: Executor<'e, Database = Postgres>>(
    executor: E,
    tenant: &Tenant,
    id: TeamId,
) -> Result<Team, KatError> {
    sqlx::query_as("SELECT id, space_id, name FROM teams WHERE workspace_id=$1 AND id=$2")
        .bind(tenant.workspace_id())
        .bind(id)
        .fetch_optional(executor)
        .await?
        .ok_or(KatError::NotFound)
}

pub async fn get(pool: &PgPool, tenant: &Tenant, id: TeamId) -> Result<TeamView, KatError> {
    get_on(pool, tenant, id).await
}

pub async fn get_on<'e, E: Executor<'e, Database = Postgres>>(
    executor: E,
    tenant: &Tenant,
    id: TeamId,
) -> Result<TeamView, KatError> {
    let rows = sqlx::query("SELECT t.id AS team_id,t.space_id AS team_space_id,t.name AS team_name,k.*,
        ARRAY(SELECT s.space_id FROM kat_spaces s WHERE s.kat_id=k.id AND s.workspace_id=k.workspace_id ORDER BY s.space_id) AS spaces
        FROM teams t LEFT JOIN team_members m ON m.team_id=t.id AND m.workspace_id=t.workspace_id AND m.active
        LEFT JOIN kats k ON k.id=m.kat_id AND k.workspace_id=m.workspace_id
        WHERE t.workspace_id=$1 AND t.id=$2 ORDER BY m.is_lead DESC,m.position,k.id")
        .bind(tenant.workspace_id()).bind(id).fetch_all(executor).await?;
    let first = rows.first().ok_or(KatError::NotFound)?;
    let team = Team {
        id: first.try_get("team_id")?,
        space_id: first.try_get("team_space_id")?,
        name: first.try_get("team_name")?,
    };
    let mut members = Vec::new();
    for row in rows {
        if row.try_get::<Option<KatId>, _>("id")?.is_some() {
            members.push(Kat::from_row(&row)?);
        }
    }
    Ok(TeamView { team, members })
}

/// Read active, granted team members in roster order within the caller's transaction.
pub async fn eligible_members(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: TeamId,
) -> Result<Vec<Kat>, KatError> {
    let team = get_team(&mut *conn, tenant, id).await?;
    Ok(sqlx::query_as("SELECT d.*, ARRAY(SELECT s.space_id FROM kat_spaces s WHERE s.kat_id=d.id AND s.workspace_id=d.workspace_id ORDER BY s.space_id) AS spaces FROM kats d JOIN team_members m ON m.kat_id=d.id AND m.workspace_id=d.workspace_id WHERE m.workspace_id=$1 AND m.team_id=$2 AND m.active AND d.status='active' AND EXISTS (SELECT 1 FROM kat_spaces s WHERE s.workspace_id=d.workspace_id AND s.kat_id=d.id AND s.space_id=$3) ORDER BY m.is_lead DESC, m.position, d.id")
        .bind(tenant.workspace_id()).bind(id).bind(team.space_id).fetch_all(conn).await?)
}

pub async fn list(pool: &PgPool, tenant: &Tenant, space: SpaceId) -> Result<Vec<Team>, KatError> {
    tenancy::space::get(pool, tenant, space).await?;
    Ok(sqlx::query_as(
        "SELECT id, space_id, name FROM teams WHERE workspace_id=$1 AND space_id=$2 ORDER BY id",
    )
    .bind(tenant.workspace_id())
    .bind(space)
    .fetch_all(pool)
    .await?)
}

pub async fn create(
    pool: &PgPool,
    tenant: &Tenant,
    space: SpaceId,
    new: NewTeam,
    lock_timeout: std::time::Duration,
) -> Result<TeamId, KatError> {
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        super::lock_workspace(&mut tx, tenant).await?;
        let id = create_on(&mut tx, tenant, space, new, false).await?;
        tx.commit().await?;
        Ok(id)
    })
    .await
}

async fn create_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space: SpaceId,
    new: NewTeam,
    is_default: bool,
) -> Result<TeamId, KatError> {
    super::check_space(conn, tenant, space).await?;
    let name = super::validate_name(&new.name)?;
    let mut seen = HashSet::new();
    let members: Vec<KatId> = std::iter::once(new.lead)
        .chain(new.members)
        .filter(|id| seen.insert(*id))
        .collect();
    let ids: Vec<_> = members.iter().map(|id| id.0).collect();
    sqlx::query("SELECT id FROM kats WHERE workspace_id=$1 AND id=ANY($2) ORDER BY id FOR SHARE")
        .bind(tenant.workspace_id())
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
    let mut names = HashSet::new();
    for member in &members {
        let kat = kat::get_on(&mut *conn, tenant, *member).await?;
        if !kat.spaces.contains(&space) {
            return Err(KatError::KatNotInSpace);
        }
        super::validate_name(&kat.name)?;
        if !names.insert(super::name_key(&kat.name)) {
            return Err(KatError::KatNameConflict);
        }
    }
    let id = TeamId::new();
    sqlx::query(
        "INSERT INTO teams (id, workspace_id, space_id, name, is_default) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(tenant.workspace_id())
    .bind(space)
    .bind(name)
    .bind(is_default)
    .execute(&mut *conn)
    .await?;
    for (position, member) in members.into_iter().enumerate() {
        sqlx::query("INSERT INTO team_members (workspace_id, team_id, kat_id, is_lead, position) VALUES ($1,$2,$3,$4,$5)")
            .bind(tenant.workspace_id()).bind(id).bind(member).bind(member == new.lead)
            .bind(i32::try_from(position).map_err(|_| KatError::RequestInvalid)?).execute(&mut *conn).await?;
    }
    Ok(id)
}

pub async fn create_default(
    pool: &PgPool,
    tenant: &Tenant,
    space: SpaceId,
    locale: Locale,
) -> Result<TeamId, KatError> {
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_configured_lock_timeout(&mut tx).await?;
        let id = create_default_on(&mut tx, tenant, space, locale).await?;
        tx.commit().await?;
        Ok(id)
    })
    .await
}

pub(super) fn default_name(locale: Locale) -> &'static str {
    match locale {
        Locale::En => "Default team",
        Locale::Th => "ทีมเริ่มต้น",
    }
}

pub(crate) async fn create_default_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space: SpaceId,
    locale: Locale,
) -> Result<TeamId, KatError> {
    super::lock_workspace(conn, tenant).await?;
    super::check_space(conn, tenant, space).await?;
    let existing = sqlx::query_scalar(
        "SELECT id FROM teams WHERE workspace_id=$1 AND space_id=$2 AND is_default",
    )
    .bind(tenant.workspace_id())
    .bind(space)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let mut members = Vec::new();
    for new in super::roles::default_roster(locale) {
        members.push(kat::hire_on(conn, tenant, space, new).await?);
    }
    let lead = members.remove(0);
    create_on(
        conn,
        tenant,
        space,
        NewTeam {
            name: default_name(locale).into(),
            lead,
            members,
        },
        true,
    )
    .await
}

pub async fn add_member(
    pool: &PgPool,
    tenant: &Tenant,
    id: TeamId,
    member: KatId,
    is_lead: bool,
    lock_timeout: std::time::Duration,
) -> Result<(), KatError> {
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        super::lock_workspace(&mut tx, tenant).await?;
        let team = get_team(&mut *tx, tenant, id).await?;
        sqlx::query("SELECT id FROM kats WHERE workspace_id=$1 AND id=$2 FOR SHARE")
            .bind(tenant.workspace_id())
            .bind(member)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(KatError::NotFound)?;
        let kat = kat::get_on(&mut *tx, tenant, member).await?;
        if !kat.spaces.contains(&team.space_id) {
            return Err(KatError::KatNotInSpace);
        }
        super::validate_name(&kat.name)?;
        let current: Vec<(KatId, String, bool)> = sqlx::query_as("SELECT d.id, d.name, m.is_lead FROM team_members m JOIN kats d ON d.id=m.kat_id AND d.workspace_id=m.workspace_id WHERE m.workspace_id=$1 AND m.team_id=$2 AND m.active")
            .bind(tenant.workspace_id()).bind(id).fetch_all(&mut *tx).await?;
        if is_lead
            && current
                .iter()
                .any(|(other, _, lead)| *lead && *other != member)
        {
            return Err(KatError::TeamLeadConflict);
        }
        if !is_lead
            && current
                .iter()
                .any(|(other, _, lead)| *lead && *other == member)
        {
            return Err(KatError::TeamLeadRequired);
        }
        if current.iter().any(|(other, name, _)| {
            *other != member && super::name_key(name) == super::name_key(&kat.name)
        }) {
            return Err(KatError::KatNameConflict);
        }
        sqlx::query("INSERT INTO team_members (workspace_id, team_id, kat_id, is_lead, position) SELECT $1,$2,$3,$4,COALESCE(MAX(position),-1)+1 FROM team_members WHERE workspace_id=$1 AND team_id=$2 ON CONFLICT (team_id,kat_id) DO UPDATE SET active=true, is_lead=EXCLUDED.is_lead")
            .bind(tenant.workspace_id()).bind(id).bind(member).bind(is_lead).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}

pub async fn remove_member(
    pool: &PgPool,
    tenant: &Tenant,
    id: TeamId,
    member: KatId,
    lock_timeout: std::time::Duration,
) -> Result<(), KatError> {
    crate::db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        crate::db::set_lock_timeout(&mut tx, lock_timeout).await?;
        super::lock_workspace(&mut tx, tenant).await?;
        get_team(&mut *tx, tenant, id).await?;
        kat::get_on(&mut *tx, tenant, member).await?;
        let is_lead: Option<bool> = sqlx::query_scalar("SELECT is_lead FROM team_members WHERE workspace_id=$1 AND team_id=$2 AND kat_id=$3 AND active")
            .bind(tenant.workspace_id()).bind(id).bind(member).fetch_optional(&mut *tx).await?;
        if is_lead == Some(true) {
            return Err(KatError::TeamLeadRequired);
        }
        sqlx::query("UPDATE team_members SET active=false WHERE workspace_id=$1 AND team_id=$2 AND kat_id=$3 AND active")
            .bind(tenant.workspace_id()).bind(id).bind(member).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    })
    .await
}
