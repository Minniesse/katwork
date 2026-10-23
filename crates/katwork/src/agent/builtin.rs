use super::{Tool, ToolCtx, ToolError, ToolOutput};
use crate::{
    chat::{Author, message},
    ids::MessageId,
    models::ToolSpec,
    types::{Question, SideEffect},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct ReadThread;
pub struct AskUser;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[serde(default = "default_limit")]
    limit: u16,
    before: Option<MessageId>,
}
fn default_limit() -> u16 {
    20
}
#[async_trait]
impl Tool for ReadThread {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_thread".into(),
            description: "Read recent messages from this thread.".into(),
            parameters: json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":50,"default":20},"before":{"type":"string","format":"uuid"}},"additionalProperties":false}),
        }
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Read
    }
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = serde_json::from_value(args)
            .map_err(|_| ToolError::InvalidArgs("read_thread".into()))?;
        if !(1..=50).contains(&args.limit) {
            return Err(ToolError::InvalidArgs("limit".into()));
        }
        let mut page = message::list(
            &ctx.pool,
            &ctx.tenant,
            ctx.thread_id,
            args.before,
            args.limit + 1,
        )
        .await
        .map_err(|_| ToolError::Failed("read_thread".into()))?
        .messages;
        let more = page.len() > args.limit as usize;
        if more {
            page.remove(0);
        }
        let before = if more {
            page.first().map(|m| m.id)
        } else {
            None
        };
        let messages:Vec<_>=page.into_iter().map(|m|json!({"id":m.id,"author":match m.author{Author::User(_)=>format!("user:{}",m.author_name),Author::Kat(_)=>format!("kat:{}",m.author_name),Author::System=>"system".into()},"body":m.body,"created_at":m.created_at})).collect();
        Ok(ToolOutput::Text(
            json!({"messages":messages,"next_before":before}).to_string(),
        ))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskArgs {
    question: String,
    options: Vec<String>,
}
#[async_trait]
impl Tool for AskUser {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask_user".into(),
            description: "Ask a person a question. The first option is recommended.".into(),
            parameters: json!({"type":"object","properties":{"question":{"type":"string","minLength":1,"maxLength":500},"options":{"type":"array","maxItems":4,"items":{"type":"string","minLength":1,"maxLength":100}}},"required":["question","options"],"additionalProperties":false}),
        }
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Read
    }
    async fn call(&self, _: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: AskArgs =
            serde_json::from_value(args).map_err(|_| ToolError::InvalidArgs("ask_user".into()))?;
        if !(1..=500).contains(&a.question.chars().count())
            || a.options.len() > 4
            || a.options
                .iter()
                .any(|s| !(1..=100).contains(&s.chars().count()))
        {
            return Err(ToolError::InvalidArgs("ask_user".into()));
        }
        Ok(ToolOutput::Ask(Question {
            question: a.question,
            options: a.options,
        }))
    }
}
