use super::*;
use crate::{
    config::Config,
    types::{ModelTier, SpacePolicy},
};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPrice {
    pub input_micros_per_mtok: u64,
    pub output_micros_per_mtok: u64,
}
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider_id: String,
    pub model: String,
    pub tier: ModelTier,
    pub capabilities: Capabilities,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
    pub price: Option<TokenPrice>,
}
#[derive(Clone)]
pub struct Selected {
    pub provider: Arc<dyn ModelProvider>,
    pub entry: ModelEntry,
}
pub struct Registry {
    entries: Vec<Selected>,
}
// Byte-level BPE models; initial defaults verified against provider catalogs 2026-10-23.
const MODELS: [(ModelTier, u32, &str, &str); 3] = [
    (
        ModelTier::Fast,
        2048,
        "gpt-4.1-nano",
        "claude-haiku-4-5-20251001",
    ),
    (
        ModelTier::Standard,
        4096,
        "gpt-4.1-mini",
        "claude-sonnet-4-6",
    ),
    (ModelTier::Strong, 8192, "gpt-4.1", "claude-opus-4-6"),
];
impl Registry {
    pub fn from_entries(entries: Vec<(ModelEntry, Arc<dyn ModelProvider>)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(entry, provider)| Selected { provider, entry })
                .collect(),
        }
    }
    pub fn from_config(config: &Config) -> Self {
        let mut entries = Vec::new();
        if config.openai.is_none() && config.anthropic.is_none() && config.demo {
            let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::demo());
            for (tier, max_output_tokens, _, _) in MODELS {
                entries.push((
                    ModelEntry {
                        provider_id: provider.id().into(),
                        model: "demo".into(),
                        tier,
                        capabilities: provider.capabilities(),
                        max_output_tokens,
                        temperature: None,
                        price: None,
                    },
                    provider.clone(),
                ));
            }
        }
        for (id, env) in [
            ("openai", config.openai.as_ref()),
            ("anthropic", config.anthropic.as_ref()),
        ] {
            let Some(env) = env else {
                continue;
            };
            let mut configured = Vec::new();
            for (tier, max_output_tokens, openai, anthropic) in MODELS {
                let model = if id == "openai" { openai } else { anthropic };
                let provider = if id == "openai" {
                    RigProvider::openai(env, model, OpenAiApi::Chat)
                } else {
                    RigProvider::anthropic(env, model)
                };
                match provider {
                    Ok(provider) => {
                        let provider: Arc<dyn ModelProvider> = Arc::new(provider);
                        configured.push((
                            ModelEntry {
                                provider_id: provider.id().into(),
                                model: model.into(),
                                tier,
                                capabilities: Capabilities {
                                    tools: true,
                                    residency: Residency::External,
                                    synthetic: false,
                                },
                                max_output_tokens,
                                temperature: None,
                                price: None,
                            },
                            provider,
                        ));
                    }
                    Err(_) => {
                        tracing::warn!(provider_id = id, "provider_init_failed");
                        configured.clear();
                        break;
                    }
                }
            }
            entries.extend(configured);
        }
        Self::from_entries(entries)
    }
    pub fn for_tier(
        &self,
        tier: ModelTier,
        space_policy: &SpacePolicy,
    ) -> Result<Selected, ModelError> {
        let mut denied = false;
        for selected in self.entries.iter().filter(|s| s.entry.tier == tier) {
            if selected.entry.capabilities.residency == Residency::External
                && !space_policy.allow_external
            {
                denied = true;
                continue;
            }
            return Ok(selected.clone());
        }
        Err(if denied {
            ModelError::ResidencyDenied
        } else {
            ModelError::ProviderUnavailable
        })
    }
}
