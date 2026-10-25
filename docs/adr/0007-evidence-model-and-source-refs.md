# 0007. Evidence model: claims, source refs and a verification ladder that only moves down

- Status: Accepted (2026-10-24), with the sub-project 2a plan, which the founder approved that day. Revised the same day after four review rounds (generated sources, subject words, judge providers, deletion; the full roll-up, R18's judge rule, instruction-like and low-contrast sources, qualifiers and periods, refinement alternatives, the deletion protocol; quotes that cut a figure, per-character hidden text, formula-only cache distrust; paint that cannot be resolved fails closed, text over images is suspect, rich-text cells, the hidden-text blind spots, a pass that races a deletion, uncited figures recorded by the commit path)
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

- **`source_spans`**: one row per block a document reader extracted from an artifact version (a paragraph or its split piece, a PDF text block, a spreadsheet cell, an OCR line). The harness mints them when a read tool reads the block, hidden blocks included (their text is never shown to the model). A span stores its artifact version, its `SourceRef`, the block's exact text, the text's SHA-256, the extractor's name and version (for OCR, the engine and its version too), and quality flags: `generated` (the artifact is a Kat's output), `hidden` (a person viewing the file would not see the text, with the reason), `instruction_like` (the text reads like an instruction to an AI, with the pattern), `suspect_text_layer`. Minting the same block again returns the same span, and a mint whose text hash differs from the stored row fails closed.
- **`citations`**: a span narrowed by the `cite` tool to an exact quote inside it, with the narrowed `SourceRef`. A citation belongs to one job.
- **`claims`**: a statement in an artifact version: its text, its kind (`number`, `date`, `quote` or `fact`), where it sits in the artifact, and whether the writer cited it. Every quantity and date in an artifact's text outside a cited claim is also recorded, as an uncited claim, by the writers' shared commit path from the content it was given, so no renderer decides what is recorded.
- **`claim_refs`**: a claim's citations, in order, each with a frozen copy of the citation's `SourceRef`.
- **`claim_checks`**: append-only. One row per check per verification pass: the method (`resolve`, `quote`, `qualifier`, `number`, `date`, `cell`, `arithmetic`, `amount_words`, `judge`; `human` is reserved for sub-project 3), the verdict (`supported`, `unsupported`, `contradicted`, `inconclusive`), the basis (`exact`, `derived`, `judged`, `none`), a stable detail code, the values compared, the verifier version, the origin (`harness` in sub-project 2a), whether the check is advisory, and, for a judge, the provider, the model and whether it shares the writer's provider.
- **`evidence_reports`**: append-only. One row per verification pass of an artifact version, with an explicit outcome: `report` (the roll-up of every claim with its checks, and the missing-evidence list) or `unavailable` (the pass could not run, with a code). The report a person reads is rendered from this row, never written by a model (spec §7.1).

Rows are never deleted. The one exception to "never updated" is deletion of a file (below).

### Statuses

The spec's three statuses stay as the roll-up of one claim's checks in one pass. Advisory checks never take part. The rules apply in this order, and the plan's `roll_up` (Task 10) implements exactly this list, with one table-test row per rule:

1. **failed** when any deterministic check (quote, number, date, cell, arithmetic, amount words) is `contradicted`. A judge alone never fails a claim (founder decision R18);
2. otherwise **unverified**, code `uncited`, when the writer did not cite it;
3. otherwise **unverified** when any ref did not resolve with full quality (`stale`, `source_generated`, `hidden_source`, `instruction_like_source`, `suspect_text_layer`, `source_deleted`, `ocr_unaligned`, `ocr_low_confidence`);
4. otherwise **unverified** when any deterministic check is `unsupported`;
5. otherwise **passed**, code `checked`, when at least one deterministic check is `supported` with an exact or derived basis, **no qualifier, number, date, cell or arithmetic check is `inconclusive`**, and no judge check is `contradicted` or `unsupported`. So a matching number beside an ambiguous date, an untrusted cell cache or a dropped "ไม่เกิน" stays unverified;
6. otherwise **unverified**, with `judge_contradicted`, `judge_unsupported` or `judged_supported` when a judge ran, else the first `inconclusive` code, else `needs_review`.

A judge's `contradicted`, like any advisory disagreement, adds the visible flag "AI found a possible mismatch", which never changes the status. Every unverified claim, uncited ones included, is on the missing-evidence list with its detail code.

"Passed" means: the value, and the subject words around it (entity, metric, period, and any negation or comparator), were found at the cited place of a file a person supplied, in text the readers judge a person can see (within the blind spots listed under Citing) that does not read as an instruction to an AI.

### SourceRef

A `SourceRef` is a W3C Web Annotation `SpecificResource`. Its `source` is `urn:katwork:artifact:<artifact_id>:v<version>`, and KatWork adds the artifact version's SHA-256, the extractor's name and version, the quality flags, and, where they apply, page boxes, cell facts and the table's unit context.

Its `selector` is one chain of `FragmentSelector`s, each link refining its parent's selection, down to a **block** (a paragraph or its piece, a PDF block, a cell, an OCR line). A narrowed ref's block link is refined by exactly two selectors, `[TextQuoteSelector, TextPositionSelector]`, which the W3C model defines as alternatives that select the same text ("if more than 1 is given, then they are considered to be alternatives that will result in the same selection", [refinedBy](https://www.w3.org/TR/annotation-model/#refinement-of-selection)). Both work inside the block's own text: the position counts its code points, and the quote's `exact`, `prefix` and `suffix` (up to 32 code points each) are all taken from it. So the quote's context lies inside the selection it refines, and every offset has one stated coordinate space:

| Source | Chain (outer → inner), then the two alternatives | Coordinates of the alternatives |
|---|---|---|
| PDF | `FragmentSelector` RFC 8118 `page=N` → `FragmentSelector` `urn:katwork:selector:block:v1` `ordinal=K` | code points of block K's text; boxes are rectangles in that page's PDF user space |
| XLSX | `FragmentSelector` `urn:katwork:selector:xlsx-sheet:v1` `sheetId=3;name=Budget` → `FragmentSelector` RFC 7111 `cell=row,col` (1-based) | code points of the cell's text as the reader produced it; the A1 form (`Budget!B7`), cached value, displayed value, formula, number format, date system and the row and column header text are cell facts beside the selector |
| DOCX | `FragmentSelector` `urn:katwork:selector:docx-paragraph:v1` `part=word/document.xml;p=17;paraId=1A2B3C4D;piece=0` | code points of that piece; `p` is the paragraph's ordinal among all `w:p` of that part in document order, `piece` the split index of a paragraph longer than 1,500 code points |
| Scan or image | `FragmentSelector` Media Fragments `xywh=pixel:x,y,w,h` on page N | code points of the OCR line; the box is in source-image pixels, origin top left |
| Plain text | `FragmentSelector` `urn:katwork:selector:block:v1` `ordinal=K` (the K-th paragraph) | code points of paragraph K's text |

Offsets never split a grapheme cluster; any other unit is converted at the edge.

```json
{
  "type": "SpecificResource",
  "source": "urn:katwork:artifact:01928c3e-…:v1",
  "sha256": "ab12…",
  "extractor": { "name": "pdf-text", "version": 1 },
  "quality": { "generated": false, "hidden": false, "instructionLike": false, "suspectTextLayer": false },
  "selector": {
    "type": "FragmentSelector", "conformsTo": "https://www.rfc-editor.org/rfc/rfc8118", "value": "page=3",
    "refinedBy": [{
      "type": "FragmentSelector", "conformsTo": "urn:katwork:selector:block:v1", "value": "ordinal=42",
      "refinedBy": [
        { "type": "TextQuoteSelector", "exact": "รายได้รวม 1,250 ล้านบาท", "prefix": "ปี 2567 ", "suffix": " เพิ่มขึ้น" },
        { "type": "TextPositionSelector", "start": 8, "end": 31 }
      ]
    }]
  },
  "boxes": [ { "page": 3, "rect": [72.0, 410.5, 301.2, 424.0] } ],
  "unitContext": "หน่วย: ล้านบาท"
}
```

### Citing

- Kats cite only through the `cite` tool, and only spans the harness minted for files the run may read. `cite` takes a span id and an optional quote, checks that the quote occurs **exactly once** in the span's text after Thai-aware normalization (a repeated quote must be lengthened), and returns a citation id. A Kat never writes a locator, a page number or a cell address that KatWork treats as one.
- Writer tools accept claims only as `{text, kind, cite: [citation ids]}`. An unknown citation id, or one minted for another job, is refused as invalid arguments.
- **A Kat's own output is not a source.** A span over a version of a Kat-output artifact is `generated`, and a claim citing it is at most unverified, with code `source_generated`. Otherwise a Kat could write a figure into a file, read it back, cite it and "pass". Sub-project 3 may add a transitive rule: such a citation counts when the output's own claim at that place passed.
- **Hidden text is not a source, and the model never reads what the readers find hidden.** A span over text a person would not see is `hidden`, and a claim citing it is at most unverified, with code `hidden_source`: PDF text in render mode 3 or 7, painted in a `Separation /None` colour, outside the CropBox or the clipping path, in an optional-content layer that is off by default, nearly transparent, under 4 pt in height or width, with a contrast ratio below 1.2 against what lies beneath it (white or near-white on white, in any common colour space, `ICCBased` included, as seen through its own alpha and any translucent area drawn over it), covered by a later opaque shape, image or shading, or painted in a way the reader cannot resolve (a pattern or gradient fill, `Lab`, a tint transform it does not evaluate, a soft mask, a blend mode other than Normal, including those of a transparency group it sits in), judged character by character, so a hidden figure inside a visible line leaves that line. When visibility cannot be decided, the text is hidden (fail closed), with one exception: PDF and PPTX text whose background cannot be known (an image or picture, a shading or pattern, an area of unknown colour, a shape that is not a rectangle, a translucent or blended area beneath it, or, in PPTX, paint outside the reader's allowlist, such as an outline, an effect, a wide line, a geometry whose fill may not lie under the text or 3D, and text that may lie outside its shape) keeps its text, because the server never decodes images, but its span is suspect and a claim citing it is at most unverified (`suspect_text_layer`). XLSX cells in hidden rows, columns and sheets, under 4 pt, with a low-contrast font, or with an empty number format such as `;;;` are hidden; a rich-text cell's hidden runs are dropped from every fact of the cell, its stored value included; a hidden cell never supplies a header or a unit to a visible one. DOCX vanished, tiny and low-contrast runs (judged against every colour Word may show: a page colour its print layout does not display counts as both that colour and white, and a theme colour the reader cannot resolve as both its stated value and the default Office theme's), and text in a namespace the reader does not understand, are dropped from the paragraph's text. PPTX text on a hidden slide, in a hidden shape or group or off the slide is hidden, and tiny, transparent, unresolvable and low-contrast PPTX runs (judged against their highlight, their fill or one a layout, master, style or table gives them, composited through its alpha, a shape beneath them and the slide's background, and, for a link, in each colour it may be drawn in) are dropped from the paragraph's text. `read_file` mints spans for hidden blocks but returns only a count of them to the model.
- **What the hidden-text rules cannot see**, so a person may miss text the model reads: glyphs that draw nothing (a font with empty outlines, a Type 3 glyph that paints nothing or sets its own colour), a ToUnicode map that disagrees with the glyphs drawn, annotation appearances drawn over text, the true shape of a clipping path that is not a rectangle, the pixels of an image beneath text (such text is suspect, above), a stroked line or outline drawn over text (strokes are not counted as covering, because a stroke's bounding box would cover the text inside every table border), PDF text covered only in part or by several areas none of which covers its whole box, a PPTX shape drawn over a text shape, a shape or picture beneath only part of a PPTX text box, a glow, shadow, soft edge or line of an earlier shape that reaches under a text box the shape does not lie under, text that wrapping, a long line or an indent carries outside its box, glyphs that draw nothing in a font the deck embeds, and text a text box clips by its overflow setting, and in DOCX a `w14:textFill` with no fill, character scaling by `w:w`, a text box placed off the page, and a header, footer or footnote part that is never shown, and in XLSX a number-format colour such as `[White]`, conditional formatting that whitens a font, a row of zero height and a shape or chart drawn over a cell. Comparing a page's rendering with its text layer (render it, read it by OCR, flag words the render lacks) would catch several of these; it is deferred until the OCR service exists (the sub-project 2a plan, Task 40 onward). Text in these blind spots is treated as visible: it can support a claim like any visible text unless another rule caps it, and an instruction-like span never can (below).
- **Instruction-like text is not a source.** A span whose text matches a small English and Thai lexicon of AI-directed instructions ("ignore … instructions", "system prompt", "ไม่ต้องสนใจคำสั่ง", "ให้ตอบว่า"), or holds Unicode tag characters or bidi controls, is `instruction_like`; a claim citing it is at most unverified (`instruction_like_source`), and it never reaches a judge. The lexicon only lowers a status, so its false positives cost a "needs review", never a pass (ADR-0010).

### Verification ladder

1. **Resolve.** The artifact version exists in the claim's workspace and Space, its stored bytes still hash to the ref's SHA-256, the file was not deleted, and the span's text is where the ref says it is. When the extractor version changed since minting, the block is extracted again and the quote relocated. A ref that does not resolve, or whose quality is not full (generated, hidden, instruction-like, suspect text layer, deleted, low-confidence OCR), leaves its claim unverified with its code, never failed: the source moved or cannot be trusted, it did not disagree. The deterministic checks below therefore run only on refs that resolved with full quality.
2. **Deterministic checks**, pure Rust, no model:
   - **Numbers and dates are checked whatever kind the writer gave the claim.** A claim whose text holds a quantity is number-checked; one that holds a date is date-checked.
   - **Subject words.** A number or date passes only if the cited quote (for a cell: also its row and column headers, sheet name and unit context) contains every subject word of the claim (entity and metric), with only the claim's quantity and dates cut out and a short stop-word list dropped. Single letters and single Thai syllables stay, because they name entities ("Entity B", "ฝ่าย ข"); a Latin word must match a whole word of the source. A missing word gives `subject_not_in_source`.
   - **Periods are compared by meaning, never as strings.** Years and periods found by the date parser are compared as values: BE 2567 and CE 2024 are one year, `ปีงบประมาณ 2569` and `FY2026` one fiscal year. A different period of the same kind is `period_mismatch` (contradicted); a fiscal year against a calendar year is `period_kind_differs`, and against an unmarked year `period_ambiguous` (both inconclusive).
   - **A quote never cuts a figure.** `cite` widens a quote that starts or ends inside a number or a date to the whole token, and a ref whose quote still cuts one (an older citation, a relocated quote) gives `quote_cuts_number` (inconclusive), so "งบประมาณ 12" quoted from "งบประมาณ 12,500 บาท" cannot pass.
   - **Qualifiers must match.** A negation or comparator (ไม่, ไม่เกิน, ไม่ต่ำกว่า, อย่างน้อย, ประมาณ, เกือบ, not, no more than, at least, about, approximately, under, over, up to and the rest of the plan's list), compared by meaning between the claim and the cited quote with 10 code points of context on each side, that one has and the other lacks gives `qualifier_mismatch` (inconclusive), for quotes and numbers alike. So "อนุมัติงบประมาณ" quoted from "ไม่อนุมัติงบประมาณ" cannot pass.
   - Years are not quantities when they are part of a period ("ปี 2567", "FY2569").
   - Also: quote containment on normalized text; numbers in Thai and Arabic digits, Thai number words and every scale (พัน to ล้านล้าน), a unit or scale stated in a table header (`หน่วย: ล้านบาท`, or `รายได้ (ล้านบาท)` in a column or row header), accounting negatives, cell number formats including trailing-comma scaling, compared at the claim's displayed precision; Thai and Buddhist-Era dates and fiscal years; arithmetic (sum, difference, ratio, share, percentage change) as one aggregate comparison over the cited operands; amounts in words against the figure beside them. An ambiguous reading (a two-digit year, an unmarked fiscal year, a confidence below 0.8) gives `inconclusive`, never a guess.
3. **A judge**, only for **disputed** claims: English claims whose refs resolved with full quality, with no contradicted check, no qualifier mismatch and no exact or derived support, and whose open codes are `paraphrase`, `needs_judge` or `subject_not_in_source`. A judge can lower or hold a claim but never lift it above unverified, and never fail it on its own (R18): `contradicted` keeps it unverified with `judge_contradicted` and the "AI found a possible mismatch" flag, `unsupported` keeps it unverified with `judge_unsupported`, `supported` keeps it unverified with `judged_supported`, and a parse failure or timeout is `inconclusive`. A span that tells the judge what to answer can therefore move nothing but a flag.

### Judge

- `ClaimJudge` is a trait. Sub-project 2a ships one implementation: a frontier-model judge called through KatWork's model gateway.
- **Only providers an admin marked as judges for the Space** receive claim or span text. Among them, the choice order is: a marked provider other than the writer's; else a marked, different model of the writer's provider, recorded on the check as `same_provider`; **never the writer's exact model**. With none marked, a disputed claim stays "needs review" with `judge_unavailable`. Changing the marked list bumps the Space's `judge_policy_version`, and the judge rereads it before each attempt, without a row lock, in the transaction that authorizes that attempt (it ends before the request is sent, so a lock would add nothing, and taking the Space row after the task and Kat rows would break the lock order), so a provider an admin unmarks receives nothing more.
- **Thai claims are never sent to a judge.** A claim whose text or cited spans contain Thai script stays on the "needs review" path. Any future advisory source (for example a qualified local model) may add a flag, "AI found a possible mismatch", which never changes the status.
- The judge sees only the claim and its cited span texts, as data, and answers from a closed set, once (one vote, temperature 0). The calibration protocol measures the same configuration.
- Every judge attempt is authorized like a model call of the run (the Kat's authority, the run's deadline and token budget) immediately before it is sent.
- **Calibration is preregistered.** A protocol file fixes the judge, its prompt hash, the vote count, the labels (`supported`, `contradicted`, `unverifiable`), the metrics and the pass thresholds before any judge number is recorded. In calibration a parse failure counts as a wrong answer and stays in the denominator; in the product it leaves the claim unchanged. Calibration measures a judge from another provider than the system under test; the product's same-provider fallback is recorded per check so it can be audited separately.
- No local NLI model in sub-project 2a. A local judge is another `ClaimJudge` implementation, added only after it passes the same protocol.

### Deletion (amends the append-only rule)

An admin can delete a file (PDPA). One audited path then, **in one transaction serialized with every reader, citer and writer**:
- takes the file's artifact row `FOR NO KEY UPDATE` right after the workspace row and before any other work, and checks inside that lock that no unfinished job uses the file. Every transaction that mints spans from a file, cites one of its spans or records a claim citing it first takes the same row `FOR SHARE` and refuses a deleted file, so a read either commits before the deletion (and is redacted by it) or finds the file deleted;
- purges **every stored copy of text derived from it**: span texts and match keys; citation quotes and display strings; the quote fields, cell facts (values, formula, row and column headers) and unit context inside every source ref that points at it; the text of every claim that cites it, in the claims and in every stored report; the values of those claims' checks; the arguments and results of tool calls, and their events, that carried its text; Kat messages that quote it; its file name; OCR lines;
- keeps every row, with ids and hashes, and marks the source `source_deleted`;
- leaves no gap for a verification pass running at the same time: before it stores anything, a pass takes the rows of the artifact it verifies and of every source it cites `FOR SHARE`; it stores nothing for an artifact found deleted, and for a source found deleted it stores `[deleted]` as the text of every claim citing it and `{"redacted": true}` as those claims' check values, as the redaction would have;
- records the work still owed (removing the blob, its derived previews and cache entries, and the re-verification of every artifact version whose claims cite it) as rows in the same transaction, which a sweep finishes after a crash.

Claims citing it become unverified with code `source_deleted`: every stored report is served with that overlay from the deletion's commit, and the re-verification pass stores the same result.

**Kat outputs that quote a deleted input** are separate artifacts. Their claims that cite the input have their text redacted in the claims and in every report, and the deletion lists those outputs for the admin to review or delete; deleting one purges it the same way. What no deletion reaches is stated where the admin deletes: copies outside KatWork, old row versions until Postgres reclaims them, and backups until they rotate; and while a job of the file's Space is still running, the deletion record keeps the removed text it must still sweep from that job.

The append-only triggers allow exactly this: setting the listed text fields to their redacted value, with `redacted = true`, inside a transaction that carries a pending deletion's audit id. Nothing else may update or delete an evidence row.

## Alternatives considered

- **Kat-supplied locators** (page numbers, cell addresses or offsets in the claim): rejected; they are post-rationalized and cannot be trusted.
- **UTF-8 byte offsets**: rejected; Thai is three bytes per character and code points match W3C.
- **A judge that can lift a claim to passed**: rejected; open judges mislabel about a quarter of English claims and none is measured on Thai.
- **A judge whose `contradicted` fails a claim**: rejected (R18). A cited span can tell the judge to answer "contradicted", which made it the one direction a hostile span could move a status; a flag keeps the signal without the lever.
- **A local NLI judge in sub-project 2a**: deferred; no Thai-capable model has evidence.
- **Checking only the number of a number claim**: rejected; it certifies the wrong entity or period.
- **Comparing years and periods as strings**: rejected; it fails a correct Buddhist-Era restatement of a Common-Era year and passes nothing it should not.
- **A position selector refined by a quote selector**: rejected; W3C refinement applies the inner selector to the outer one's selection, which would put the quote's prefix and suffix outside the text they describe. The two are alternatives within the block.
- **Hard-deleting evidence rows on file deletion**: rejected; it breaks the audit trail. Redaction keeps ids and hashes.

## Consequences

- A citation is valid by construction and its support is still checked, so "the model cited page 3" never counts as proof.
- Kats must cite the whole phrase that names a figure, not the figure alone; a bare "1,250" quote leaves the claim unverified.
- Every check is replayable: the ref, the extractor version, the values compared and the verifier version are stored, and a new pass appends rather than overwrites.
- Spans are stored per extracted block, so a large workbook adds many rows; the read tools page through blocks.
- Changing an extractor bumps its version. Old refs still resolve, by re-extraction and quote relocation, or fall to `stale`.
- A spreadsheet's cached value can disagree with its formula, and Excel would show the recalculated one. Workbooks marked for full recalculation on load are flagged, and their formula cells cannot pass until a recalculation oracle exists; their value cells are unaffected.
- Thai claims that only a judge could support stay "needs review". The missing-evidence list will be long; that is the honest state.
- The instruction lexicon and the qualifier list are small and fixed; their false positives only add to that list, and KatBench measures them on benign worlds.
- Sub-project 3 builds acceptance on these records: a contract's required-evidence scope, the `human` check, and the rule that checks written by the same Kat cap a job at `ready`.

**Note (2026-10-25).** The sub-project 2a Craft round extends this model without changing its rules: `docs/adr/0011-craft-apps-in-katwork.md` adds claims carried from one version to the next (`carried_from`) and claims lost in an edit (`lost_in_edit`, checked by the method `relocate`, code `claim_lost_in_edit`), the report outcome `not_checked` for outputs KatWork cannot read, and versions saved by a person (`saved_by_user`). The closed lists of methods and outcomes above grow by those. A person's copy of a Kat output saved in a Craft app keeps the output's role, so spans over it stay `generated`.
