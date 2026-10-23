use super::{ChatChunk, ModelError, StopKind, TimeoutPhase};
use futures::{StreamExt, stream::BoxStream};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct StreamTimeouts {
    pub first_chunk: Duration,
    pub idle: Duration,
}
impl Default for StreamTimeouts {
    fn default() -> Self {
        Self {
            first_chunk: Duration::from_secs(30),
            idle: Duration::from_secs(30),
        }
    }
}
/// Poll-owned deadlines: dropping the wrapper drops the network operation.
/// Forwards `Progress` so callers disable retries after non-text content too.
pub fn guard_stream(
    mut inner: BoxStream<'static, Result<ChatChunk, ModelError>>,
    t: StreamTimeouts,
) -> BoxStream<'static, Result<ChatChunk, ModelError>> {
    Box::pin(async_stream::stream! {
        let mut first = true;
        let mut deadline = tokio::time::Instant::now() + t.first_chunk;
        loop {
            let chunk = match tokio::time::timeout_at(deadline, inner.next()).await {
                Err(_) => {
                    yield Err(ModelError::Timeout {
                        phase: if first {
                            TimeoutPhase::FirstChunk
                        } else {
                            TimeoutPhase::Idle
                        },
                    });
                    break;
                }
                Ok(None) => {
                    yield Err(ModelError::Protocol("stream_without_done".into()));
                    break;
                }
                Ok(Some(chunk)) => chunk,
            };
            let content = match &chunk {
                Ok(ChatChunk::Usage(_)) | Err(_) => false,
                Ok(ChatChunk::TextDelta(text)) => !text.is_empty(),
                Ok(_) => true,
            };
            if content {
                first = false;
                deadline = tokio::time::Instant::now() + t.idle;
            }
            match chunk {
                Ok(ChatChunk::Done {
                    stop: StopKind::ToolUse,
                    ref turn,
                }) if turn.tool_calls.is_empty() => {
                    yield Err(ModelError::Protocol("tool_use_without_call".into()));
                    break;
                }
                Ok(ChatChunk::Done { .. }) | Err(_) => {
                    yield chunk;
                    break;
                }
                _ => yield chunk,
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTimeouts {
    pub first_chunk: Duration,
    pub idle: Duration,
    pub run_wall_clock: Duration,
}
impl ModelTimeouts {
    pub const HOSTED: Self = Self {
        first_chunk: Duration::from_secs(30),
        idle: Duration::from_secs(30),
        run_wall_clock: Duration::from_secs(90),
    };
    pub const LOCAL: Self = Self {
        first_chunk: Duration::from_secs(120),
        idle: Duration::from_secs(60),
        run_wall_clock: Duration::from_secs(300),
    };
    pub fn stream(&self) -> StreamTimeouts {
        StreamTimeouts {
            first_chunk: self.first_chunk,
            idle: self.idle,
        }
    }
}
