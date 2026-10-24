//! Atomic, deterministic message intake. The thread lock serializes receipts and routing.
use sqlx::{Connection, Postgres, Transaction};

use crate::{
    agent::{Initiator, RunFacts, StopReason},
    chat::{
        Author, Message, NewMessage, NoticeKat, NoticeMeta, Thread, UserPost, mentions, message,
    },
    db,
    error::KatError,
    events::{JobStatusEvent, KatworkEvent, MessageStoredEvent, log},
    ids::*,
    jobs::{RouteRule, Routing, TaskInput, job, quota, task},
    models::ChatMsg,
    runtime::Core,
    team::{kat, team},
    tenancy::{self, Tenant},
    types::{JobStatus, KatStatus, ReasonCode},
};

#[derive(Debug)]
pub struct Submitted {
    pub message_id: MessageId,
    pub job_id: JobId,
    pub created: bool,
    pub last_event_seq: Option<EventSeq>,
}

pub async fn submit(
    core: &Core,
    tenant: &Tenant,
    thread_id: ThreadId,
    post: UserPost,
) -> Result<Submitted, KatError> {
    let submitted = db::guarded(&core.pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, core.config.lock_timeout).await?;
        let submitted = intake(&mut tx, core, tenant, thread_id, post).await?;
        tx.commit().await?;
        Ok(submitted)
    })
    .await?;
    if let Some(seq) = submitted.last_event_seq {
        core.events.notify(thread_id, seq);
    }
    Ok(submitted)
}

/// The caller must drain its transaction or discard its connection on cancellation, as submit does.
pub async fn intake(
    tx: &mut Transaction<'_, Postgres>,
    _core: &Core,
    tenant: &Tenant,
    thread_id: ThreadId,
    post: UserPost,
) -> Result<Submitted, KatError> {
    message::validate_user_post(&post.body, post.client_msg_id.as_deref())?;
    let thread: Thread = sqlx::query_as("SELECT id,space_id,team_id,title FROM threads WHERE id=$1 AND workspace_id=$2 FOR NO KEY UPDATE")
        .bind(thread_id).bind(tenant.workspace_id()).fetch_optional(&mut **tx).await?.ok_or(KatError::NotFound)?;
    tenancy::resolve(&mut **tx, tenant.user_id(), tenant.workspace_id()).await?;
    if let Some(key) = &post.client_msg_id {
        let saved: Option<(MessageId, Option<JobId>, String)> = sqlx::query_as("SELECT id,job_id,body FROM messages WHERE workspace_id=$1 AND thread_id=$2 AND author_user_id=$3 AND client_msg_id=$4")
            .bind(tenant.workspace_id()).bind(thread_id).bind(tenant.user_id()).bind(key).fetch_optional(&mut **tx).await?;
        if let Some((message_id, job_id, body)) = saved {
            if body != post.body {
                return Err(KatError::ClientMsgIdConflict);
            }
            return Ok(Submitted {
                message_id,
                job_id: job_id.ok_or(KatError::RequestInvalid)?,
                created: false,
                last_event_seq: None,
            });
        }
    }
    let latest = job::latest_in_thread(tx, tenant, thread_id).await?;
    let parent = latest.filter(|j| j.status == Some(JobStatus::NeedsInput));
    let roster = team::get_on(&mut **tx, tenant, thread.team_id).await?;
    let eligible = team::eligible_members(tx, tenant, thread.team_id).await?;
    let names: Vec<_> = roster
        .members
        .iter()
        .map(|k| (k.id, k.name.as_str()))
        .collect();
    let (mentions, missing_spaces) = tracing::info_span!("mentions", thread_id = %thread_id)
        .in_scope(|| mentions::resolve_with_warnings(&post.body, &names));
    let mut notices = Vec::new();
    for id in &missing_spaces {
        if let Some(k) = roster.members.iter().find(|k| k.id == *id) {
            notices.push((
                ReasonCode::MentionNeedsSpace,
                vec![NoticeKat {
                    id: *id,
                    name: k.name.clone(),
                }],
                Some(*id),
            ));
        }
    }
    let mut chosen = None;
    let mut ignored = Vec::new();
    for id in mentions {
        if let Some(k) = eligible.iter().find(|k| k.id == id) {
            if chosen.is_none() {
                chosen = Some((k, RouteRule::Mention));
            } else {
                ignored.push(id);
            }
        } else if let Some(k) = roster.members.iter().find(|k| k.id == id) {
            let code = if k.status == KatStatus::Paused {
                ReasonCode::KatPaused
            } else {
                ReasonCode::KatNotInSpace
            };
            notices.push((
                code,
                vec![NoticeKat {
                    id,
                    name: k.name.clone(),
                }],
                Some(id),
            ));
        }
    }
    if chosen.is_none() {
        chosen = parent
            .as_ref()
            .filter(|p| missing_spaces.iter().all(|id| Some(*id) == p.kat_id))
            .and_then(|p| eligible.iter().find(|k| Some(k.id) == p.kat_id))
            .map(|k| (k, RouteRule::FollowUp));
    }
    if chosen.is_none() {
        chosen = eligible.first().map(|k| {
            (
                k,
                if roster.members.first().is_some_and(|lead| lead.id == k.id) {
                    RouteRule::Lead
                } else {
                    RouteRule::FirstEligible
                },
            )
        });
    }
    if !ignored.is_empty() {
        let kats = ignored
            .iter()
            .filter_map(|id| roster.members.iter().find(|k| k.id == *id))
            .map(|k| NoticeKat {
                id: k.id,
                name: k.name.clone(),
            })
            .collect();
        notices.push((ReasonCode::ExtraMentionIgnored, kats, None));
    }
    let (routed, stop) = if let Some((k, rule)) = chosen {
        let stop = quota::check(tx, k, tenant.user_id()).await?;
        let fresh = kat::get_on(&mut **tx, tenant, k.id).await?;
        // Roster edits can win while quota waits: retry before writing a stale routing decision.
        let still_member: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM team_members WHERE workspace_id=$1 AND team_id=$2 AND kat_id=$3 AND active)")
            .bind(tenant.workspace_id()).bind(thread.team_id).bind(k.id).fetch_one(&mut **tx).await?;
        if fresh.status != KatStatus::Active
            || !fresh.spaces.contains(&thread.space_id)
            || !still_member
        {
            return Err(KatError::Busy);
        }
        (Some((fresh, rule)), stop)
    } else {
        (None, Some(StopReason::NoEligibleKat))
    };
    let paused: bool = sqlx::query_scalar("SELECT paused FROM workspaces WHERE id=$1")
        .bind(tenant.workspace_id())
        .fetch_one(&mut **tx)
        .await?;
    if paused && stop.is_none() {
        notices.push((ReasonCode::WorkspacePaused, vec![], None));
    }
    // A follow-up reruns a request whose job may already have changed something. That job has
    // finished, so its tasks need no lock (and task rows would lock before the thread row).
    if let Some(parent) = &parent {
        let wrote: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE workspace_id=$1 AND job_id=$2 AND effects_started_at IS NOT NULL)")
            .bind(tenant.workspace_id()).bind(parent.id).fetch_one(&mut **tx).await?;
        if wrote {
            notices.push((ReasonCode::ReviewBeforeRerun, vec![], None));
        }
    }
    // All required existing-row locks precede the first write. New rows cannot be observed before commit.
    let mut request = message::append(
        tx,
        tenant,
        thread_id,
        NewMessage {
            id: None,
            author: Author::User(tenant.user_id()),
            body: post.body,
            client_msg_id: post.client_msg_id,
            idempotency_key: None,
            synthetic: false,
            stopped: false,
            question: None,
            meta: None,
            job_id: None,
        },
    )
    .await?
    .message;
    let job_id = match parent {
        Some(parent) => job::create_follow_up(tx, tenant, parent.id, request.id).await?,
        None => job::create(tx, tenant, thread_id, request.id).await?,
    };
    message::set_job(tx, tenant, request.id, job_id).await?;
    request.job_id = Some(job_id);
    announce(tx, tenant, thread_id, &request).await?;
    for (code, kats, id) in notices {
        let key = match id {
            Some(id) => format!("notice:{job_id}:{}:{id}", code.wire()),
            None => format!("notice:{job_id}:{}", code.wire()),
        };
        let notice = message::append(
            tx,
            tenant,
            thread_id,
            NewMessage {
                id: None,
                author: Author::System,
                body: code.wire().into(),
                client_msg_id: None,
                idempotency_key: Some(key),
                synthetic: false,
                stopped: false,
                question: None,
                meta: Some(NoticeMeta { code, kats }),
                job_id: Some(job_id),
            },
        )
        .await?
        .message;
        announce(tx, tenant, thread_id, &notice).await?;
    }
    let seq = if let Some(stop) = stop {
        job::conclude(
            tx,
            tenant,
            job_id,
            thread_id,
            routed.as_ref().map(|(k, _)| k.id),
            &job_id.to_string(),
            &RunFacts::not_run(stop),
        )
        .await?
    } else {
        let (k, rule) = routed.ok_or(KatError::RequestInvalid)?;
        let input = TaskInput {
            thread_id,
            request_message_id: request.id,
            routing: Routing { rule, ignored },
            actor: tenant.user_id(),
            initiator: Initiator::Person,
        };
        task::enqueue(
            tx,
            job_id,
            k.id,
            k.policy_version,
            &input,
            &format!("job:{job_id}:kat:{}", k.id),
        )
        .await?;
        log::append(
            tx,
            tenant.workspace_id(),
            thread_id,
            None,
            &KatworkEvent::JobStatus(JobStatusEvent {
                job_id,
                status: None,
                reason: None,
                hold_reason: None,
            })
            .into(),
        )
        .await?
    };
    Ok(Submitted {
        message_id: request.id,
        job_id,
        created: true,
        last_event_seq: Some(seq),
    })
}

async fn announce(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &Tenant,
    thread: ThreadId,
    message: &Message,
) -> Result<(), KatError> {
    log::append(
        tx,
        tenant.workspace_id(),
        thread,
        None,
        &KatworkEvent::MessageStored(MessageStoredEvent::from(message)).into(),
    )
    .await?;
    Ok(())
}

#[derive(Debug)]
pub struct History {
    pub messages: Vec<ChatMsg>,
    pub cut: u32,
}

pub fn build_history(messages: &[Message], kat_id: KatId, budget_chars: usize) -> History {
    let turns: Vec<_> = messages
        .iter()
        .filter(|m| m.author != Author::System)
        .collect();
    let mut start = turns.len();
    let mut size = 0usize;
    let mut consecutive_users = 0;
    for (i, m) in turns.iter().enumerate().rev() {
        size = size.saturating_add(m.body.chars().count());
        if m.author != Author::Kat(kat_id) {
            if consecutive_users > 0 {
                size = size.saturating_add(m.author_name.chars().count() + 3);
                if consecutive_users == 1 {
                    size = size.saturating_add(turns[i + 1].author_name.chars().count() + 2);
                }
            }
            consecutive_users += 1;
        } else {
            consecutive_users = 0;
        }
        if i + 1 != turns.len() && size > budget_chars {
            break;
        }
        start = i;
    }
    while turns
        .get(start)
        .is_some_and(|m| m.author == Author::Kat(kat_id))
    {
        start += 1;
    }
    let mut messages = Vec::new();
    for m in &turns[start..] {
        if m.author == Author::Kat(kat_id) {
            messages.push(ChatMsg::Assistant {
                content: m.body.clone(),
                tool_calls: vec![],
                replay: None,
            });
        } else if let Some(ChatMsg::User { content, name }) = messages.last_mut() {
            if let Some(author) = name.take() {
                *content = format!("{author}: {content}");
            }
            content.push_str(&format!("\n{}: {}", m.author_name, m.body));
        } else {
            messages.push(ChatMsg::User {
                content: m.body.clone(),
                name: Some(m.author_name.clone()),
            });
        }
    }
    History {
        messages,
        cut: u32::try_from(start).unwrap_or(u32::MAX),
    }
}

pub async fn load_history(
    core: &Core,
    tenant: &Tenant,
    thread_id: ThreadId,
    request_message_id: MessageId,
    kat_id: KatId,
    budget_chars: usize,
) -> Result<History, KatError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        message: Message,
        dropped: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(include_str!("pipeline_history.sql"))
        .bind(tenant.workspace_id())
        .bind(thread_id)
        .bind(request_message_id)
        .bind(i64::try_from(budget_chars).unwrap_or(i64::MAX))
        .fetch_all(&core.pool)
        .await?;
    let dropped = rows.first().ok_or(KatError::NotFound)?.dropped;
    let messages: Vec<_> = rows.into_iter().map(|r| r.message).collect();
    let mut history = build_history(&messages, kat_id, budget_chars);
    history.cut = history
        .cut
        .saturating_add(u32::try_from(dropped).unwrap_or(u32::MAX));
    Ok(history)
}
