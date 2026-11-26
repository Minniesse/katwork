# Bundled fonts

KatWork bundles two Thai families under the SIL Open Font License 1.1, and no other font: Sarabun
and Noto Sans Thai, static instances only. Every task that renders Thai uses these files (the
Typst fixtures with `typst compile --font-path crates/katwork/assets/fonts --ignore-system-fonts`,
the demo folder, the Craft image, the report renderer and the generator). TH Sarabun New and TH
SarabunPSK are not bundled until their rights check.

`OFL.txt` holds the licence once, under both families' copyright lines. `tests/fonts.rs`
(`font_bundle_hashes_match_manifest`) checks that every file in this directory is listed here, with
this SHA-256, and that no other font file is present.

| File | SHA-256 | Licence | Source |
|---|---|---|---|
| `Sarabun-Regular.ttf` | `226d4f368fbc0457990ddef2692679badfd2c1a4e89e5ac4d43c10ba7743b2f1` | OFL-1.1 | google/fonts `ofl/sarabun/Sarabun-Regular.ttf` at commit `88058c714073e72bb1b32baffc2d4ce9bc73944c` |
| `Sarabun-Bold.ttf` | `d38308ca27d067b9a1b79006337c1f9a66c29aa016d96a9d88d3801da7fa83b9` | OFL-1.1 | google/fonts `ofl/sarabun/Sarabun-Bold.ttf` at commit `88058c714073e72bb1b32baffc2d4ce9bc73944c` |
| `Sarabun-Italic.ttf` | `595a968e97a8b8179a6bde2d74d4f91b11475dcfb6cf270037d76d8ed71160ea` | OFL-1.1 | google/fonts `ofl/sarabun/Sarabun-Italic.ttf` at commit `88058c714073e72bb1b32baffc2d4ce9bc73944c` |
| `Sarabun-BoldItalic.ttf` | `5ef1b930f9a3afed5ec83374b5617cad3a89d91a8fc5e372dc028c2aa2950f8d` | OFL-1.1 | google/fonts `ofl/sarabun/Sarabun-BoldItalic.ttf` at commit `88058c714073e72bb1b32baffc2d4ce9bc73944c` |
| `NotoSansThai-Regular.ttf` | `7f890179ceabb5f34a17702271c83c015c807fc78deaf52de859f6ec8847c1cf` | OFL-1.1 | `NotoSansThai/googlefonts/ttf/NotoSansThai-Regular.ttf` in the release archive that google/fonts `ofl/notosansthai/METADATA.pb` names (see below) |
| `NotoSansThai-Bold.ttf` | `fb9a50b38abd40195acc76465fdf2f3a82a2ebdf47ef99294cf8b24ebbf33516` | OFL-1.1 | `NotoSansThai/googlefonts/ttf/NotoSansThai-Bold.ttf` in the same archive |

## Where each file comes from

- **Sarabun**: <https://github.com/google/fonts/tree/88058c714073e72bb1b32baffc2d4ce9bc73944c/ofl/sarabun>,
  the last google/fonts commit to touch the directory when the bundle was made (upstream
  <https://github.com/cadsondemak/Sarabun>, commit `854cdbc6afa002ff8c2ce6aa7b86f99c7f71c9eb`).
  Copyright 2018 The Sarabun Project Authors.
- **Noto Sans Thai**: google/fonts ships only a variable font, `ofl/notosansthai/NotoSansThai[wdth,wght].ttf`.
  Its `METADATA.pb` at google/fonts commit `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5` builds that file
  from `NotoSansThai/googlefonts/variable/` in the release archive
  <https://github.com/notofonts/thai/releases/download/NotoSansThai-v2.002/NotoSansThai-v2.002.zip>
  (4,720,990 bytes, SHA-256 `af889cc673fc714060ce5e4e088fbad32aa4c0571a19958efeaff128a22da485`;
  upstream <https://github.com/notofonts/thai>, commit `f8f3f024703f9d939d02f4e2fe16f1d5a39ca963`).
  The two files here are the static instances of the same build, from that archive's
  `NotoSansThai/googlefonts/ttf/`, copied byte for byte. Copyright 2022 The Noto Project Authors.
- **`OFL.txt`**: the licence text of google/fonts `ofl/notosansthai/OFL.txt` at commit
  `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5`, with the Sarabun copyright line of
  `ofl/sarabun/OFL.txt` at commit `88058c714073e72bb1b32baffc2d4ce9bc73944c` added above it. The
  two upstream texts differ only in those lines and in the scheme of the licence's URL.
