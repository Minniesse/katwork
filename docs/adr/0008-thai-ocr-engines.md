# 0008. Thai OCR behind an `OcrEngine` trait, with line boxes for citations

- Status: Accepted (2026-10-24), pending the 100-page Thai bake-off in the sub-project 2a plan, which confirms or overturns the engine choice
- Decision owner: Founder
- Spec: §9 (document tools), §13 (isolation), §15 (document fidelity gates), §19 item 2; amends the OCR line of §9 and the OAR-OCR pick in §2 and §5.2

## Context

Scanned Thai documents are common in the alpha's templates: signed minutes, government letters, older annual reports. A citation into a scan must land on a region a person can see (ADR-0007: Media Fragments `xywh=`), so OCR has to return text **and** line boxes.

What is known, from published sources:

- **Typhoon OCR 1.5 (2B)**, a Qwen3-VL-2B fine-tune under Apache-2.0 metadata with a separate terms notice, has the best Thai results in every comparison that includes it, including a third-party median CER of 6.2% on ThaiOCRBench reading tasks ([model card](https://huggingface.co/typhoon-ai/typhoon-ocr1.5-2b), [paper](https://arxiv.org/abs/2601.14722), [third-party comparison](https://arxiv.org/abs/2609.03595), [terms](https://opentyphoon.ai/tac)). It returns Markdown and HTML tables but **no coordinates**, and its card recommends vLLM on a GPU and warns that GGUF builds may lose accuracy.
- **PP-OCRv5 Thai** runs on CPU through ONNX Runtime, and OAR-OCR (Apache-2.0) wraps it in Rust with text-line detection ([OAR-OCR](https://github.com/GreatV/oar-ocr), [PP-OCRv5 languages](https://github.com/PaddlePaddle/PaddleOCR/blob/main/docs/version3.x/algorithm/PP-OCRv5/PP-OCRv5_multi_languages.en.md)). Its only Thai figure is the vendor's own text-line accuracy.
- **Tesseract `tha`** is everywhere, Apache-2.0, and gives hOCR line boxes, but scores far below the leaders on ThaiOCRBench full-page OCR ([Tesseract](https://github.com/tesseract-ocr/tesseract), [ThaiOCRBench](https://arxiv.org/abs/2511.04479)).
- No neutral leaderboard includes Typhoon OCR 1.5; the benchmark maintainers did not run it. Vendor numbers decide nothing on their own.

## Decision

- **An `OcrEngine` trait** in the core: an engine takes one page image and returns an `OcrPage`. Engines are chosen by what the install has, never by the model's own output.
- **Default engine where a GPU exists: Typhoon OCR 1.5 (2B)** behind an OpenAI-compatible local server (vLLM, or any server speaking that API). Because it returns no boxes, it runs together with a **line-box detector** (PP-OCR text detection through OAR-OCR) and an **aligner** that maps Typhoon's lines to detected lines by edit distance on normalized text.
- **CPU fallback: PP-OCRv5 Thai through OAR-OCR**, which gives text and line boxes itself.
- **Last resort: Tesseract `tha`**, used only when neither of the others is available.
- **Everything that runs a model is out of process.** OAR-OCR and Tesseract run in a pinned `katwork-ocr` container with no network route and an env allowlist; the VLM runs in its own server. The core talks to both over an internal network and never loads an OCR model itself.
- **An `OcrPage` evidence record** per page and engine version: the page image's SHA-256, its pixel size, and its lines, each with an id, an `xywh` box in source-image pixels, the text, the engine's confidence when it gives one, and where the box came from (`detected`, `aligned` or `none`, with the alignment score). The engine's raw Markdown is kept for display only; citations point at lines.
- **OCR text is evidence with limits.** A claim cited to an OCR line can pass only when the line has a box and either the engine's confidence or the alignment score is at least 0.8; otherwise it stays unverified (`ocr_low_confidence` or `ocr_unaligned`). A PDF page whose text layer fails the sanity check (replacement or private-use characters, impossible Thai mark sequences, fonts without a ToUnicode map) is read by OCR instead, and its spans record that.
- **The bake-off decides.** 100 Thai pages and 20 English control pages, about 70% synthetic with text and boxes known by construction (10 of them printed and phone-photographed) and about 30% real public pages whose reference text two Thai reviewers corrected; a separate 10-page handwriting stress set is reported alone. A reference is never another engine's output. ThaiOCRBench is an internal anchor only, never committed, because some of its images come from licensed commercial datasets. Metrics: character error rate on normalized text, the same with whitespace removed (the headline, because Thai has no word spaces), the spurious-space rate, number exact-match split into missed and changed, TEDS for tables, line-box hit and cover rates for citations, and seconds per page on GPU and on 4 CPU cores. One engine beats another only when the paired bootstrap interval of the page-level difference excludes zero and the gain is at least 2 points. The result is recorded in this ADR and either confirms the engines above or replaces them.
- **OCR lands last in sub-project 2a**, after the evidence model and the verifier work on digital files.

## Consequences

- Installs without a GPU get the CPU engine, with lower accuracy than a VLM; the bake-off says how much lower, and the product shows OCR-sourced claims as such.
- A GPU install runs two OCR passes per page (VLM text, detector boxes). Lines the aligner cannot place get no highlight and cannot pass.
- The Typhoon terms notice needs a legal read before customer deployments; the weights are not bundled in KatWork's images, and the operator downloads them.
- The spec's earlier "OAR-OCR reads scanned Thai documents" becomes the CPU path; the VLM path and the box requirement are new.
- Model files are pinned by SHA-256 and baked into the OCR image; nothing downloads at run time.
