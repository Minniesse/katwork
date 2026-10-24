# 0007. Evidence model: claims, source refs and a verification ladder that only moves down

- Status: Accepted (2026-10-24); amended the same day after plan review (generated sources, subject words, judge providers, deletion)
- Decision owner: Founder
- Spec: §4 (Evidence, Claim), §6.5 (evidence verifier), §7.1 (evidence model), §13 (deletion), §15 (judges), §19 items 2 and 3

## Context

Every deliverable must carry proof: each claim *passed*, *failed* or *unverified*, plus a missing-evidence list (spec §1.1 goal 5). Spec §7.1 names what a source span stores (file and version, page, sheet or cell, URL and retrieval time, a content hash and the exact text), but not how a span is addressed inside a file, who may create one, or how a check is recorded.

Four facts shape the design:

- **A valid pointer is not support, and a fluent writer can cite nothing valid.** Studies of generated reports find citations that resolve but do not support their sentence, and correct-looking citations that were added after the fact ([arXiv 2412.18004](https://arxiv.org/abs/2412.18004), [arXiv 2304.09848](https://arxiv.org/abs/2304.09848)). Resolution and support need separate checks, and the harness, not the writer, must do both.
- **A matching number is not a matching claim.** "Entity B's revenue is 1,250" citing "Entity A's revenue is 1,250" has the right number and the wrong subject. Models are good at single-element perturbations and bad at claims whose salient part is right and a secondary part is wrong ([arXiv 2604.10990](https://arxiv.org/abs/2604.10990)).
- **Model judges are weak, English-first, and favour their own output.** The best open, commercially usable grounding judges score about 75–77% balanced accuracy on the English LLM-AggreFact board ([leaderboard](https://llm-aggrefact.github.io/)); the best-documented small judges cannot read Thai at all ([MiniCheck](https://huggingface.co/lytang/MiniCheck-Flan-T5-Large)); LLM-judge agreement across languages averages a Fleiss κ of about 0.3 ([arXiv 2505.12201](https://arxiv.org/abs/2505.12201)); and models recognise and favour their own generations ([arXiv 2404.13076](https://arxiv.org/abs/2404.13076)).
- **Locator formats exist and disagree on units.** The W3C Web Annotation model gives selectors for quotes, positions and fragments, with code-point offsets, and lets one selector refine another within its parent's selection ([W3C](https://www.w3.org/TR/annotation-model/#refinement-of-selection)). PDF pages have [RFC 8118](https://www.rfc-editor.org/rfc/rfc8118), CSV-style cells [RFC 7111](https://www.rfc-editor.org/rfc/rfc7111), image regions [Media Fragments](https://www.w3.org/TR/media-frags/) `xywh=`. DOCX has no standard; Word's `w14:paraId` must be unique in its scope ([Open XML SDK](https://learn.microsoft.com/en-us/dotnet/api/documentformat.openxml.features.iparagraphidgeneratorfeature.createuniqueparagraphid?view=openxml-3.0.1)), and there is no run-level id.

## Decision

### Records

- **`source_spans`**: one row per block a document reader extracted from an artifact version (a paragraph or its split piece, a PDF text block, a spreadsheet cell, an OCR line). The harness mints them when a read tool returns the block. A span stores its artifact version, its `SourceRef`, the block's exact text, the text's SHA-256, the extractor's name and version (for OCR, the engine and its version too), and quality flags: `generated` (the artifact is a Kat's output), `hidden` (a person viewing the file would not see the text), `suspect_text_layer`. Minting the same block again returns the same span, and a mint whose text hash differs from the stored row fails closed.
- **`citations`**: a span narrowed by the `cite` tool to an exact quote inside it, with the narrowed `SourceRef`. A citation belongs to one job.
- **`claims`**: a statement in an artifact version: its text, its kind (`number`, `date`, `quote` or `fact`), where it sits in the artifact, and whether the writer cited it. Every quantity and date the writer tools find in an artifact's text outside a cited claim is also recorded, as an uncited claim.
- **`claim_refs`**: a claim's citations, in order, each with a frozen copy of the citation's `SourceRef`.
- **`claim_checks`**: append-only. One row per check per verification pass: the method (`resolve`, `quote`, `number`, `date`, `cell`, `arithmetic`, `amount_words`, `judge`; `human` is reserved for sub-project 3), the verdict (`supported`, `unsupported`, `contradicted`, `inconclusive`), the basis (`exact`, `derived`, `judged`, `none`), a stable detail code, the values compared, the verifier version, the origin (`harness` in sub-project 2a), whether the check is advisory, and, for a judge, the provider, the model and whether it shares the writer's provider.
- **`evidence_reports`**: append-only. One row per verification pass of an artifact version, with an explicit outcome: `report` (the roll-up of every claim with its checks, and the missing-evidence list) or `unavailable` (the pass could not run, with a code). The report a person reads is rendered from this row, never written by a model (spec §7.1).

Rows are never deleted. The one exception to "never updated" is deletion of a file (below).

### Statuses

The spec's three statuses stay as the roll-up of one claim's checks in one pass; advisory checks never take part:

- **failed** when any check is `contradicted`;
- otherwise **passed** when the claim is cited, every ref resolved with full quality, at least one deterministic check is `supported` with an exact or derived basis, and no check is `unsupported`;
- otherwise **unverified**. Every unverified claim, uncited ones included, is on the missing-evidence list with its detail code.

"Passed" means: the value, and the subject words around it (entity, metric, period), were found at the cited place of a file a person supplied, in text a person can see.

### SourceRef

A `SourceRef` is a W3C Web Annotation `SpecificResource`. Its `source` is `urn:katwork:artifact:<artifact_id>:v<version>`, and KatWork adds the artifact version's SHA-256, the extractor's name and version, the quality flags, and, where they apply, page boxes, cell facts and the table's unit context.

Its `selector` is one chain, each link refining its parent's selection, so every offset has one stated coordinate space:

| Source | Chain (outer → inner) | Coordinates |
|---|---|---|
| PDF | `FragmentSelector` RFC 8118 `page=N` → `FragmentSelector` `urn:katwork:selector:block:v1` `ordinal=K` → `TextPositionSelector` → `TextQuoteSelector` | positions count code points of block K's text; boxes are rectangles in that page's PDF user space |
| XLSX | `FragmentSelector` `urn:katwork:selector:xlsx-sheet:v1` `sheetId=3;name=Budget` → `FragmentSelector` RFC 7111 `cell=row,col` (1-based) → `TextPositionSelector` → `TextQuoteSelector` | positions count code points of the cell's text as the reader produced it; the A1 form (`Budget!B7`), cached value, displayed value, formula, number format, date system and the row and column header text are cell facts beside the selector |
| DOCX | `FragmentSelector` `urn:katwork:selector:docx-paragraph:v1` `part=word/document.xml;p=17;paraId=1A2B3C4D;piece=0` → `TextPositionSelector` → `TextQuoteSelector` | `p` is the paragraph's ordinal among all `w:p` of that part in document order; `piece` is the split index of a paragraph longer than 1,500 code points; positions count code points of that piece |
| Scan or image | `FragmentSelector` Media Fragments `xywh=pixel:x,y,w,h` on page N → `TextPositionSelector` → `TextQuoteSelector` | box in source-image pixels, origin top left; positions count code points of the OCR line |
| Plain text | `TextPositionSelector` → `TextQuoteSelector` | positions count code points of the whole decoded file |

The quote carries up to 32 code points of `prefix` and `suffix`. Offsets never split a grapheme cluster; any other unit is converted at the edge.

```json
{
  "type": "SpecificResource",
  "source": "urn:katwork:artifact:01928c3e-…:v1",
  "sha256": "ab12…",
  "extractor": { "name": "pdf-text", "version": 1 },
  "quality": { "generated": false, "hidden": false, "suspectTextLayer": false },
  "selector": {
    "type": "FragmentSelector", "conformsTo": "https://www.rfc-editor.org/rfc/rfc8118", "value": "page=3",
    "refinedBy": {
      "type": "FragmentSelector", "conformsTo": "urn:katwork:selector:block:v1", "value": "ordinal=42",
      "refinedBy": {
        "type": "TextPositionSelector", "start": 12, "end": 36,
        "refinedBy": { "type": "TextQuoteSelector", "exact": "รายได้รวม 1,250 ล้านบาท", "prefix": "ปี 2567 ", "suffix": " เพิ่มขึ้น" }
      }
    }
  },
  "boxes": [ { "page": 3, "rect": [72.0, 410.5, 301.2, 424.0] } ],
  "unitContext": "หน่วย: ล้านบาท"
}
```

### Citing

- Kats cite only through the `cite` tool, and only spans the harness minted for files the run may read. `cite` takes a span id and an optional quote, checks that the quote occurs **exactly once** in the span's text after Thai-aware normalization (a repeated quote must be lengthened), and returns a citation id. A Kat never writes a locator, a page number or a cell address that KatWork treats as one.
- Writer tools accept claims only as `{text, kind, cite: [citation ids]}`. An unknown citation id, or one minted for another job, is refused as invalid arguments.
- **A Kat's own output is not a source.** A span over a version of a Kat-output artifact is `generated`, and a claim citing it is at most unverified, with code `source_generated`. Otherwise a Kat could write a figure into a file, read it back, cite it and "pass". Sub-project 3 may add a transitive rule: such a citation counts when the output's own claim at that place passed.
- **Hidden text is not a source.** A span over text a person would not see (DOCX vanished text is never extracted; PDF text in render mode 3, outside the CropBox, in an optional-content layer that is off by default, or under 1 pt; XLSX hidden rows, columns and sheets) is `hidden`, and a claim citing it is at most unverified, with code `hidden_source`.

### Verification ladder

1. **Resolve.** The artifact version exists in the claim's workspace and Space, its stored bytes still hash to the ref's SHA-256, the file was not deleted, and the span's text is where the ref says it is. When the extractor version changed since minting, the block is extracted again and the quote relocated. A ref that does not resolve, or whose quality is not full (generated, hidden, suspect text layer, low-confidence OCR), leaves its claim unverified with its code, never failed: the source moved or cannot be trusted, it did not disagree.
2. **Deterministic checks**, pure Rust, no model:
   - **Numbers and dates are checked whatever kind the writer gave the claim.** A claim whose text holds a quantity is number-checked; one that holds a date is date-checked.
   - **Subject words.** A number or date passes only if the cited quote (for a cell: also its row and column headers, sheet name and unit context) contains every subject word of the claim (entity, metric, period), compared after `match_key` with the number masked. A missing word gives `subject_not_in_source`; a different period found in the quote gives `period_mismatch` (contradicted).
   - Years are not quantities when they are part of a period ("ปี 2567", "FY2569").
   - Also: quote containment on normalized text; numbers in Thai and Arabic digits, Thai number words and every scale (พัน to ล้านล้าน), a unit stated in a table header, accounting negatives, cell number formats including trailing-comma scaling, compared at the claim's displayed precision; Thai and Buddhist-Era dates and fiscal years; arithmetic (sum, difference, ratio, share, percentage change) as one aggregate comparison over the cited operands; amounts in words against the figure beside them. An ambiguous reading (a two-digit year, an unmarked fiscal year, a confidence below 0.8) gives `inconclusive`, never a guess.
3. **A judge**, only for **disputed** claims: English claims whose refs resolved with full quality, with no contradicted check and no exact or derived support, and whose open codes are `paraphrase`, `needs_judge` or `subject_not_in_source`. A judge can lower or hold a claim but never lift it above unverified: `contradicted` makes it failed, `unsupported` keeps it unverified, `supported` keeps it unverified with `judged_supported`, and a parse failure or timeout is `inconclusive`.

### Judge

- `ClaimJudge` is a trait. Sub-project 2a ships one implementation: a frontier-model judge called through KatWork's model gateway.
- **Only providers an admin marked as judges for the Space** receive claim or span text. Among them, the choice order is: a marked provider other than the writer's; else a marked, different model of the writer's provider, recorded on the check as `same_provider`; **never the writer's exact model**. With none marked, a disputed claim stays "needs review" with `judge_unavailable`.
- **Thai claims are never sent to a judge.** A claim whose text or cited spans contain Thai script stays on the "needs review" path. Any future advisory source (for example a qualified local model) may add a flag, "AI found a possible mismatch", which never changes the status.
- The judge sees only the claim and its cited span texts, as data, and answers from a closed set, once (one vote, temperature 0). The calibration protocol measures the same configuration.
- Every judge attempt is authorized like a model call of the run (the Kat's authority, the run's deadline and token budget) immediately before it is sent.
- **Calibration is preregistered.** A protocol file fixes the judge, its prompt hash, the vote count, the labels (`supported`, `contradicted`, `unverifiable`), the metrics and the pass thresholds before any judge number is recorded. In calibration a parse failure counts as a wrong answer and stays in the denominator; in the product it leaves the claim unchanged. Calibration measures a judge from another provider than the system under test; the product's same-provider fallback is recorded per check so it can be audited separately.
- No local NLI model in sub-project 2a. A local judge is another `ClaimJudge` implementation, added only after it passes the same protocol.

### Deletion (amends the append-only rule)

An admin can delete a file (PDPA). One audited path then:
- removes the blob;
- purges every text derived from it: span texts and match keys, citation quotes and display strings, the quote fields of `claim_refs` and of stored reports, OCR lines, and the results of tool calls and their events that carried its text;
- keeps every row, with ids and hashes, and marks the source `source_deleted`; claims citing it become unverified with code `source_deleted` in the next pass and in every report rendered after deletion.

The append-only triggers allow exactly this: setting those text fields to their redacted value inside a transaction that carries the deletion's audit id. Nothing else may update or delete an evidence row.

## Alternatives considered

- **Kat-supplied locators** (page numbers, cell addresses or offsets in the claim): rejected; they are post-rationalized and cannot be trusted.
- **UTF-8 byte offsets**: rejected; Thai is three bytes per character and code points match W3C.
- **A judge that can lift a claim to passed**: rejected; open judges mislabel about a quarter of English claims and none is measured on Thai.
- **A local NLI judge in sub-project 2a**: deferred; no Thai-capable model has evidence.
- **Checking only the number of a number claim**: rejected; it certifies the wrong entity or period.
- **Hard-deleting evidence rows on file deletion**: rejected; it breaks the audit trail. Redaction keeps ids and hashes.

## Consequences

- A citation is valid by construction and its support is still checked, so "the model cited page 3" never counts as proof.
- Kats must cite the whole phrase that names a figure, not the figure alone; a bare "1,250" quote leaves the claim unverified.
- Every check is replayable: the ref, the extractor version, the values compared and the verifier version are stored, and a new pass appends rather than overwrites.
- Spans are stored per extracted block, so a large workbook adds many rows; the read tools page through blocks.
- Changing an extractor bumps its version. Old refs still resolve, by re-extraction and quote relocation, or fall to `stale`.
- A spreadsheet's cached value can disagree with its formula, and Excel would show the recalculated one. Workbooks marked for full recalculation on load are flagged, and their cells cannot pass until a recalculation oracle exists.
- Thai claims that only a judge could support stay "needs review". The missing-evidence list will be long; that is the honest state.
- Sub-project 3 builds acceptance on these records: a contract's required-evidence scope, the `human` check, and the rule that checks written by the same Kat cap a job at `ready`.
