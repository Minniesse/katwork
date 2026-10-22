use super::*;
use async_trait::async_trait;
use futures::{
    StreamExt,
    stream::{self, BoxStream},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone)]
pub enum FakeTurn {
    Chunks(Vec<ChatChunk>),
    Error(ModelError),
    Hang,
    ChunksThenHang(Vec<ChatChunk>),
}
#[derive(Clone)]
pub struct FakeProvider {
    state: Arc<Mutex<State>>,
    demo: bool,
}
struct State {
    turns: VecDeque<FakeTurn>,
    received: Vec<ChatRequest>,
}
impl FakeProvider {
    pub fn scripted(turns: Vec<FakeTurn>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                turns: turns.into(),
                received: vec![],
            })),
            demo: false,
        }
    }
    pub fn demo() -> Self {
        Self {
            demo: true,
            ..Self::scripted(vec![])
        }
    }
    pub fn received(&self) -> Vec<ChatRequest> {
        self.state.lock().unwrap().received.clone()
    }
}
#[async_trait]
impl ModelProvider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            residency: Residency::Local,
            synthetic: self.demo,
        }
    }
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatChunk, ModelError>>, ModelError> {
        let mut state = self.state.lock().unwrap();
        let turn = if self.demo {
            let thai = req
                .messages
                .iter()
                .rev()
                .find_map(|msg| match msg {
                    ChatMsg::User { content, .. } => Some(content),
                    _ => None,
                })
                .is_some_and(|text| text.chars().any(|c| ('\u{0e00}'..='\u{0e7f}').contains(&c)));
            let content = if thai {
                "สวัสดี ฉันคือผู้ช่วย AI ของทีม พร้อมช่วยคุณวางแผนและจัดการงาน"
            } else {
                "Hello! I am your team's AI assistant. I can help you plan and organize your work."
            }
            .to_owned();
            FakeTurn::Chunks(vec![
                ChatChunk::TextDelta(content.clone()),
                ChatChunk::Usage(Usage::default()),
                ChatChunk::Done {
                    stop: StopKind::EndTurn,
                    turn: AssistantTurn {
                        content,
                        tool_calls: vec![],
                        replay: None,
                    },
                },
            ])
        } else {
            state
                .turns
                .pop_front()
                .unwrap_or(FakeTurn::Error(ModelError::ProviderUnavailable))
        };
        state.received.push(req);
        match turn {
            FakeTurn::Error(error) => Err(error),
            FakeTurn::Chunks(chunks) => Ok(stream::iter(chunks.into_iter().map(Ok)).boxed()),
            FakeTurn::Hang => Ok(stream::pending().boxed()),
            FakeTurn::ChunksThenHang(chunks) => Ok(stream::iter(chunks.into_iter().map(Ok))
                .chain(stream::pending())
                .boxed()),
        }
    }
}
