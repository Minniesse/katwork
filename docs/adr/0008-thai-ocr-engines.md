# 0008. Thai OCR behind an `OcrEngine` trait, with line boxes for citations

- Status: Accepted (2026-10-24), amended the same day after plan review; the 100-page Thai bake-off in the sub-project 2a plan confirms or overturns the engine choice
- Decision owner: Founder
- Spec: §5.2 (documents and inference picks), §9 (document tools), §9.1 (the PdfCraft row), §11 (model gateway), §13 (isolation), §15 (document fidelity gates), §19 item 2

## Context

Scanned Thai documents are common in the alpha's templates: signed minutes, government letters, older annual reports. A citation into a scan must land on a region a person can see (ADR-0007: Media Fragments `xywh=`), so OCR has to return text **and** line boxes.

What is known, from published sources:

- **PP-OCRv5 Thai** runs on CPU through ONNX Runtime, and OAR-OCR (Apache-2.0) wraps it in Rust with text-line detection ([OAR-OCR](https://github.com/GreatV/oar-ocr), [PP-OCRv5 languages](https://github.com/PaddlePaddle/PaddleOCR/blob/main/docs/version3.x/algorithm/PP-OCRv5/PP-OCRv5_multi_languages.en.md)). It is what the spec already names (OAR-OCR). Its only Thai figure is the vendor's own text-line accuracy.
- **Typhoon OCR 1.5 (2B)**, a Qwen3-VL-2B fine-tune under Apache-2.0 metadata with a separate terms notice, has the best Thai results in every comparison that includes it, including a third-party median CER of 6.2% on ThaiOCRBench reading tasks ([model card](https://huggingface.co/typhoon-ai/typhoon-ocr1.5-2b), [paper](https://arxiv.org/abs/2601.14722), [third-party comparison](https://arxiv.org/abs/2609.03595), [terms](https://opentyphoon.ai/tac)). It returns Markdown and HTML tables but **no coordinates**.
- **mistral.rs**, the spec's local inference server, loads the Qwen3-VL architecture: its loader maps `Qwen3VLForConditionalGeneration` ([multimodal_loaders.rs](https://github.com/EricLBuehler/mistral.rs/blob/master/mistralrs-core/src/pipeline/loaders/multimodal_loaders.rs)), which is the architecture Typhoon OCR 1.5 declares ([config.json](https://huggingface.co/typhoon-ai/typhoon-ocr1.5-2b/blob/main/config.json)), and it serves images through its OpenAI-compatible API ([Qwen3-VL server example](https://github.com/EricLBuehler/mistral.rs/blob/master/docs/src/content/docs/examples/server/qwen3-vl.md)). That the fine-tuned weights load and read Thai correctly is not yet shown by a run.
- **Tesseract `tha`** is everywhere, Apache-2.0, and gives hOCR line boxes, but scores far below the leaders on ThaiOCRBench full-page OCR ([Tesseract](https://github.com/tesseract-ocr/tesseract), [ThaiOCRBench](https://arxiv.org/abs/2511.04479)).
- No neutral leaderboard includes Typhoon OCR 1.5; vendor numbers decide nothing on their own.

## Decision

- **An `OcrEngine` trait** in the core: an engine takes one page image and **the calling run's context** (its real deadline, the Space policy, its identity and authority, its token budget and ledger handles, and cancellation) and returns an `OcrPage`. Engines are chosen by configuration, Space residency and the remaining time budget, never by the model's own output.
- **Default engine: PP-OCRv5 Thai through OAR-OCR**, which gives text and line boxes itself.
- **Last resort: Tesseract `tha`**, used when PP-OCRv5 is unavailable or times out.
- **Typhoon OCR 1.5 (2B) is opt-in** (`KATWORK_OCR_TYPHOON=on`), off until the founder's legal read of Typhoon's terms passes. It is served by **mistral.rs**; if the load test in the plan shows mistral.rs cannot serve the model, a vLLM service becomes an optional GPU profile (a founder-approved spec change). Either way it goes **through KatWork's model gateway**: a registry entry with the `vision_ocr` capability and its declared residency; before each attempt the run's authority is checked and its token budget must cover the attempt, or nothing is sent; every attempt records a `model_calls` row with purpose `ocr` and the run's ids, and charges the run's budget. Because it returns no boxes, it runs together with PP-OCRv5's line detection and an **aligner** that maps its lines to detected lines by edit distance on normalized text.
- **Everything that runs a model or a native tool is out of process.** OAR-OCR, Tesseract and PDF rasterization (hayro, Apache-2.0 or MIT) run in a pinned `katwork-ocr` container with no network route and an env allowlist; mistral.rs runs as its own server. The core never loads an OCR model.
- **An `OcrPage` evidence record** per page, engine and engine version: the page image's SHA-256, its pixel size, and its lines, each with an id, an `xywh` box in source-image pixels, the text, the engine's confidence when it gives one, and where the box came from (`detected`, `aligned` or `none`, with the alignment score). Span identity includes the engine and its version, so a page read again by another engine gets new spans.
- **OCR text is evidence with limits.** A claim cited to an OCR line can pass only when the line has a box and either the engine's confidence or the alignment score is at least 0.8; otherwise it stays unverified (`ocr_low_confidence`, `ocr_unaligned`).
- **Number caps are an explicit rule.** `ocr/number-caps.json` lists, per engine and version, the share of numbers the bake-off found silently changed. An engine above 1%, or with no bake-off result yet, caps every OCR-sourced number claim at unverified (`ocr_number_capped`). The file is generated from the bake-off results and checked in.
- **One deadline.** OCR in a read tool fits inside the call's real deadline (the earlier of the tool timeout and the run's own deadline), measured when OCR starts: each engine gets a time slice, time for the fallback is reserved, and pages not read within the call are returned as "continue at page N".
- **A PDF page whose text layer fails the sanity check** (replacement or private-use characters, impossible Thai mark sequences, fonts without a ToUnicode map) or has no text is read by OCR instead, and its spans record that.
- **The bake-off decides.** 100 Thai pages and 20 English control pages; about 70% synthetic with text and boxes known by construction (10 of them printed and phone-photographed) and about 30% real public pages whose reference text two Thai reviewers corrected (added when the reviewers start; until then the bake-off reports synthetic pages only); a separate 10-page handwriting stress set reported alone. A reference is never another engine's output. ThaiOCRBench is an internal anchor only, never committed, because some of its images come from licensed commercial datasets. Metrics: character error rate on normalized text, the same with whitespace removed (the headline, because Thai has no word spaces), the spurious-space rate, number exact-match split into missed and changed, names exact-match, TEDS for tables, line-box hit and cover rates, and seconds per page. One engine beats another only when the paired bootstrap interval of the page-level difference excludes zero and the gain is at least 2 points. The result is recorded here.
- **OCR lands last in sub-project 2a**, after the meeting-minutes demo, the judge, KatBench and the Craft sidecar.

## Alternatives considered

- **A VLM alone, without boxes**: rejected; a citation into a scan must point at a region.
- **Tesseract alone**: rejected as the default; its Thai accuracy is far below the others.
- **Hosted OCR APIs**: rejected for the default; documents stay on the install (spec §13). A Space that allows hosted providers could add one later through the gateway.
- **PdfCraft's OCR**: rejected; it reads Latin script only, and its weights are CC-BY-SA-4.0.
- **Typhoon as the default now**: rejected until its terms are read.

## Consequences

- Installs without a GPU get the CPU engine, with lower accuracy than a VLM; the bake-off says how much lower, and the product shows OCR-sourced claims as such.
- Until the bake-off records an engine's number-change rate, OCR-sourced number claims cannot pass.
- With Typhoon on, a page gets two passes (VLM text, detector boxes). Lines the aligner cannot place get no highlight and cannot pass.
- Typhoon's weights are never bundled in KatWork's images; the operator downloads them into the mistral.rs volume.
- Spec §9's OCR line, §5.2's documents and inference picks and §9.1's PdfCraft row change to match.
- PP-OCRv5's model files and Tesseract's `tha` data are pinned by SHA-256 and baked into the `katwork-ocr` image; nothing downloads at run time.
