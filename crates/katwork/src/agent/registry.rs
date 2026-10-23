use super::{Tool, builtin};
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
}
