use super::{Tool, ToolDeps, builtin};
use crate::{
    artifacts::tools::{ListFiles, ReadFile},
    docs::tools::{WriteDocx, WriteXlsx},
    evidence::cite::Cite,
};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("duplicate_tool: {name}")]
pub struct DuplicateTool {
    pub name: String,
}
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}
impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateTool> {
        let name = tool.spec().name;
        match self.tools.entry(name) {
            Entry::Occupied(entry) => Err(DuplicateTool {
                name: entry.key().clone(),
            }),
            Entry::Vacant(entry) => {
                entry.insert(tool);
                Ok(())
            }
        }
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(id).cloned()
    }
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        registry
            .register(Arc::new(builtin::ReadThread))
            .expect("distinct built-in names");
        registry
            .register(Arc::new(builtin::AskUser))
            .expect("distinct built-in names");
        registry
    }
    /// `builtin()` plus the document tools; later tasks add theirs here.
    pub fn standard(deps: &ToolDeps) -> Self {
        let mut registry = Self::builtin();
        registry
            .register(Arc::new(ListFiles))
            .expect("distinct standard names");
        registry
            .register(Arc::new(ReadFile::new(deps.clone())))
            .expect("distinct standard names");
        registry
            .register(Arc::new(Cite))
            .expect("distinct standard names");
        registry
            .register(Arc::new(WriteDocx::new(deps.clone())))
            .expect("distinct standard names");
        registry
            .register(Arc::new(WriteXlsx::new(deps.clone())))
            .expect("distinct standard names");
        registry
    }
}
