# 0005. One local OpenAI-compatible model provider

- Status: Accepted (2026-10-23)
- Spec: §2 (models), §11 (models), §13 (data residency)

## Context

Every Space declares whether hosted providers may see its data. The gateway knew only hosted providers and the keyless demo provider, so a local-only Space could run only in demo mode, with synthetic replies, and the sensitive work that local-only Spaces exist for had no real model. Most local model servers, mistral.rs among them, speak the OpenAI Chat Completions API. Local models also start and stream more slowly than hosted APIs.

## Decision

- An operator configures one local server with `KATWORK_LOCAL_BASE_URL` and `KATWORK_LOCAL_MODEL`, plus an optional `KATWORK_LOCAL_API_KEY`. Without a key, requests carry no `Authorization` header.
- The local provider uses the same Rig adapter (`OpenAiApi::Chat`), the same bounded HTTP client and the same adapter rules as the hosted providers (ADR-0001).
- It is registered as `Residency::Local` for every tier, after the hosted providers. A Space that allows hosted providers gets a hosted entry when one is configured; a local-only Space gets the local entry. A configured local provider turns off the demo fallback.
- Each model entry carries its own stream timeouts. Local entries get 120 s to the first content chunk and 60 s between content chunks (initial defaults); hosted entries keep 30 s and 30 s. Every attempt is still capped by the run's remaining time.
- At startup the server logs one line per tier, naming the entry a hosted-allowed Space gets and the one a local-only Space gets, with no key or URL.

## Consequences

- A local-only Space runs real work outside demo mode.
- Residency comes from the variable the operator set, never from the URL. KatWork does not check that the endpoint is inside the install's network, so pointing `KATWORK_LOCAL_BASE_URL` at a remote service sends local-only data there. The README states it.
- One local model serves every tier, so the tiers differ only in their output-token caps until per-tier local models are needed.
- With the 90 s run wall clock, a local model's wait for first content is at most 90 s; a slow local model may need that limit raised.
- The conservative input charge for a missing usage count assumes a tokenizer that emits at most one token per UTF-8 byte. KatWork cannot verify that for an operator's model.
- The operator chooses the model. KatWork still recommends a local model only after it is qualified on Thai office tasks (§11).
