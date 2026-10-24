# 0007. Evidence model: claims, source refs and a verification ladder that only moves down

- Status: Accepted (2026-10-24)
- Decision owner: Founder
- Spec: §4 (Evidence, Claim), §6.5 (evidence verifier), §7.1 (evidence model), §19 items 2 and 3

## Context

Every deliverable must carry proof: each claim *passed*, *failed* or *unverified*, plus a missing-evidence list (spec §1.1 goal 5). Spec §7.1 names what a source span stores (file and version, page, sheet or cell, URL and retrieval time, a content hash and the exact text), but not how a span is addressed inside a file, who may create one, or how a check is recorded.

Three facts shape the design:

- **A valid pointer is not support, and a fluent writer can cite nothing valid.** Published studies of generated reports find citations that resolve but do not support their sentence, and correct-looking citations that were added after the fact ([arXiv 2412.18004](https://arxiv.org/abs/2412.18004), [arXiv 2304.09848](https://arxiv.org/abs/2304.09848)). Resolution and support need separate checks, and the harness, not the writer, must do both.
- **Model judges are weak and English-first.** The best open, commercially usable grounding judges score about 75–77% balanced accuracy on the English LLM-AggreFact board ([leaderboard](https://llm-aggrefact.github.io/)). The best-documented small judges share a T5 vocabulary with no Thai characters, so they cannot read Thai at all ([MiniCheck](https://huggingface.co/lytang/MiniCheck-Flan-T5-Large)). No Thai grounding benchmark exists.
- **Locator formats exist and disagree on units.** The W3C Web Annotation model gives selectors for quotes, positions and fragments ([W3C](https://www.w3.org/TR/annotation-model/)), with code-point offsets; one vendor API uses UTF-8 bytes. PDF pages have [RFC 8118](https://www.rfc-editor.org/rfc/rfc8118), CSV-style cells have [RFC 7111](https://www.rfc-editor.org/rfc/rfc7111), image regions have [Media Fragments](https://www.w3.org/TR/media-frags/) `xywh=`. DOCX has no standard; Word's `w14:paraId` is a paragraph id that must be unique in its scope ([Open XML SDK](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.features.iparagraphidgeneratorfeature.createuniqueparagraphid?view=openxml-3.0.1)), and there is no run-level id.

## Decision

### Records

- **`source_spans`**: one row per block a document reader extracted from an artifact version (a paragraph, a PDF text block, a spreadsheet cell, an OCR line). The harness mints them when a read tool returns the block. A span stores its artifact version, its `SourceRef`, the block's exact text, the text's SHA-256 and the extractor's name and version. The same block read twice gets the same span.
- **`citations`**: a span narrowed by the `cite` tool to an exact quote inside it, with the narrowed `SourceRef`. A citation belongs to one job.
- **`claims`**: a statement in an artifact version: its text, its kind (`number`, `date`, `quote` or `fact`), where it sits in the artifact, and whether the writer cited it. Every number and date the verifier finds in an artifact's text outside a cited claim is also recorded, as an uncited claim.
- **`claim_refs`**: a claim's citations, in order, each with a frozen copy of the citation's `SourceRef`.
- **`claim_checks`**: append-only. One row per check per verification pass: the method (`resolve`, `quote`, `number`, `date`, `cell`, `arithmetic`, `amount_words`, `judge`; `human` is reserved for sub-project 3), the verdict (`supported`, `unsupported`, `contradicted`, `inconclusive`), a stable detail code, the values compared, the verifier version, the origin (`harness` in sub-project 2a; `template`, `person` and `kat` arrive with acceptance contracts), and, for a judge, the provider and model. A check is never updated or deleted; a new pass appends new rows.
- **`evidence_reports`**: one row per verification pass of an artifact version: the roll-up of every claim and the missing-evidence list, computed by code from the checks of that pass. The report a person reads is rendered from this row, never written by a model (spec §7.1).

### Statuses

The spec's three statuses stay as the roll-up of one claim's checks in one pass:

- **failed** when any check is `contradicted`;
- otherwise **passed** when the claim is cited, every ref resolved, and at least one deterministic check is `supported` with an exact or derived basis, and no check is `unsupported`;
- otherwise **unverified**. Every unverified claim, uncited ones included, is on the missing-evidence list with its detail code.

### SourceRef

A `SourceRef` is a W3C Web Annotation `SpecificResource` whose `source` is `urn:katwork:artifact:<artifact_id>:v<version>`, extended with the artifact version's SHA-256, the extractor's name and version, and, where they apply, page boxes, cell facts and the table's unit context. Its selectors depend on the file type:

| Source | Selector |
|---|---|
| PDF | `FragmentSelector` conforming to RFC 8118, `page=N` (1-based physical page), plus boxes: rectangles in PDF user space on that page |
| XLSX | `FragmentSelector` conforming to RFC 7111, `cell=row,col` (1-based), plus the sheet's `sheetId` and name, the A1 form (`Budget!B7`) for display, the cached value, the formula text, the number format and the workbook's date system |
| DOCX | `FragmentSelector` conforming to `urn:katwork:selector:docx-paragraph:v1`: the part path, the paragraph's ordinal among all `w:p` elements of that part in document order, and `w14:paraId` when the file has one |
| Scan or image | `FragmentSelector` conforming to Media Fragments, `xywh=pixel:x,y,w,h` in source-image pixels, plus the page, the OCR engine and its version |
| Plain text | `TextQuoteSelector` and `TextPositionSelector` over the whole file's text |

Every narrowed ref also carries a `TextQuoteSelector` (`exact`, plus up to 32 code points of `prefix` and `suffix`) and a `TextPositionSelector`, as `refinedBy` of the fragment selector, relative to the block's text. Offsets count Unicode code points and never split a grapheme cluster; any other unit is converted at the edge.

```json
{
  "type": "SpecificResource",
  "source": "urn:katwork:artifact:01928c3e-…:v1",
  "sha256": "ab12…",
  "extractor": { "name": "pdf-text", "version": 1 },
  "selector": {
    "type": "FragmentSelector",
    "conformsTo": "https://www.rfc-editor.org/rfc/rfc8118",
    "value": "page=3",
    "refinedBy": [
      { "type": "TextQuoteSelector", "exact": "รายได้รวม 1,250 ล้านบาท", "prefix": "ปี 2567 ", "suffix": " เพิ่มขึ้น" },
      { "type": "TextPositionSelector", "start": 412, "end": 436 }
    ]
  },
  "boxes": [ { "page": 3, "rect": [72.0, 410.5, 301.2, 424.0] } ],
  "unitContext": "หน่วย: ล้านบาท"
}
```

### Citing

- Kats cite only through the `cite` tool, and only spans the harness minted for files the run may read. `cite` takes a span id and an optional quote, checks that the quote occurs in the span's text after Thai-aware normalization, and returns a citation id. A Kat never writes a locator, a page number or a cell address that KatWork treats as one.
- Writer tools accept claims only as `{text, kind, cite: [citation ids]}`. An unknown citation id, or one minted for another job, is refused as invalid arguments.

### Verification ladder

1. **Resolve.** The artifact version exists in the claim's workspace and Space, its stored bytes still hash to the ref's SHA-256, and the span's text is where the ref says it is. When the extractor version changed since the span was minted, the block is extracted again and the quote relocated. A ref that does not resolve leaves its claim unverified with code `stale`, never failed: the source moved, it did not disagree.
2. **Deterministic checks**, pure Rust, no model: quote containment on normalized text, numbers (Thai and Arabic digits, Thai number words and scale words, a unit stated in a table header, accounting negatives, compared at the claim's displayed precision), Thai and Buddhist-Era dates and fiscal years, spreadsheet cells against their cached value, arithmetic (sum, difference, ratio, share, percentage change) recomputed from the cited values, and amounts in words against the figure beside them. An ambiguous reading (a two-digit year, an unmarked fiscal year, a confidence below 0.8) gives `inconclusive`, never a guess.
3. **A judge**, only for disputed claims: those whose refs resolved and whose deterministic checks are all `inconclusive`. A judge can lower or hold a claim but never lift it above unverified: `contradicted` makes it failed, `unsupported` keeps it unverified, `supported` keeps it unverified with the detail code `judged_supported`, and a parse failure or timeout is `inconclusive`.

### Judge

- `ClaimJudge` is a trait. Sub-project 2a ships one implementation: a frontier-model judge called through KatWork's model gateway, using only providers the Space allows, with a model and provider other than the one that wrote the artifact. When no such model is configured, the claim stays unverified with `judge_unavailable`.
- The judge sees only the claim and its cited span texts, as data, and answers from a closed set. Span text is untrusted; because the judge can only lower a claim, text that tries to talk the judge into `supported` changes nothing.
- No local NLI model in sub-project 2a. A local judge is another `ClaimJudge` implementation, added only after it passes the Thai claim gold set.

## Consequences

- A citation is valid by construction and its support is still checked, so "the model cited page 3" never counts as proof.
- Every check is replayable: the ref, the extractor version, the values compared and the verifier version are stored, and a new pass appends rather than overwrites.
- Spans are stored per extracted block, so a large workbook adds many rows; the read tools page through blocks instead of minting a whole file at once.
- Changing an extractor bumps its version. Old refs still resolve, by re-extraction and quote relocation, or fall to `stale`.
- Thai claims that only a judge could support stay unverified until a Thai-qualified judge exists. The missing-evidence list will be long; that is the honest state.
- Sub-project 3 builds acceptance on these records: a contract's required-evidence scope, the `human` check, and the rule that checks written by the same Kat cap a job at `ready`.
