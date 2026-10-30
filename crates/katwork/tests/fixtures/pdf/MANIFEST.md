# PDF reader fixtures

Every committed file the PDF reader is tested or fuzzed with is listed here with its SHA-256.
Hostile PDFs are never committed as fixtures: `tests/docs_pdf.rs` builds each one inside the test
that uses it, with `tests/support/pdf_build.rs`, and the fuzz seeds are rebuilt the same way.
`pdf_fixtures_match_the_manifest` checks that the files present are exactly these, with these
hashes.

## Typst fixtures

Made with the Typst CLI 0.15.1 from the `.typ` source beside each, from the repository root:

```sh
typst compile --font-path crates/katwork/assets/fonts --ignore-system-fonts \
  crates/katwork/tests/fixtures/pdf/<name>.typ
```

Each source sets `date: none`, so two builds give the same bytes. They use only the bundled
fonts; the two files their pages draw from are listed with the hashes `FONTS.md` records.

| File | SHA-256 | What |
|---|---|---|
| `fixtures/pdf/thai.typ` | `03edbcf3f89206bb48e0556022e237ebd896790d4c8e789461b20ad21e62b9e6` | Thai paragraphs in Sarabun and Noto Sans Thai: SARA AM, a tone mark over an above vowel, `1,250`, a paragraph wrapping over four lines |
| `fixtures/pdf/thai.pdf` | `907f15b545543a6de529647dc1f924142d61b3b7089e3d2943545554be870b6a` | `thai.typ`, compiled |
| `fixtures/pdf/white.typ` | `7c506252625845712ca71d209521baf6a8027deaa8f0625551f1d4dfb70924d8` | black text, `#text(fill: white)` and `#text(fill: rgb("#F8F8F8"))` |
| `fixtures/pdf/white.pdf` | `2d3f7c54fb38c7e93bed8245dc653eddb46048e47bf95d7002f7886a73513acb` | `white.typ`, compiled |
| `fixtures/pdf/gradient.typ` | `84a42694ab8f483eb0458b808e101a210843c2d91dfc207485e29f9b08f23bee` | `#text(fill: gradient.linear(white, white))[9,999]` |
| `fixtures/pdf/gradient.pdf` | `2d0059537b03daa96cbebcdff3abe96e44f40c312894e848d6bd64e8d77a4bc6` | `gradient.typ`, compiled |
| `assets/fonts/Sarabun-Regular.ttf` | `226d4f368fbc0457990ddef2692679badfd2c1a4e89e5ac4d43c10ba7743b2f1` | the font of every fixture's body text |
| `assets/fonts/NotoSansThai-Regular.ttf` | `7f890179ceabb5f34a17702271c83c015c807fc78deaf52de859f6ec8847c1cf` | the font of `thai.typ`'s last paragraph |

## Fuzz seeds (`fuzz/corpus/pdf/`)

Made by `cargo test -p katwork --test docs_pdf -- --ignored --exact regenerate_pdf_fuzz_seeds`,
which rewrites the corpus and this table from `tests/support/pdf_build.rs`.

| Seed | SHA-256 | What |
|---|---|---|
| `fuzz/corpus/pdf/simple.pdf` | `08adc9b54e3bd7577337d91a99b4cb709c71e80808b2b17eeec64f4e080f34d7` | two pages of text, Helvetica and an identity-mapped Type 0 font |
| `fuzz/corpus/pdf/thai-typst.pdf` | `907f15b545543a6de529647dc1f924142d61b3b7089e3d2943545554be870b6a` | the Typst Thai fixture (tests/fixtures/pdf/thai.pdf) |
| `fuzz/corpus/pdf/page-tree-cycle.pdf` | `1db452698a1b18fa29ea7e70d165b90fd0f4bfa1458e48f928892fdd351a897e` | a Pages node that is its own kid |
| `fuzz/corpus/pdf/cid-width-range.pdf` | `db610594f244732eaeb82b29a96b0ab75f5d1a1dca3b9dc2bdb7306b894cfeb6` | a /W entry 0 4294967295 500 |
| `fuzz/corpus/pdf/predictor-columns.pdf` | `1662ebdcb783317c480aaddace1e6defef48f1fd9a8a137830383b78d15dd6da` | a PNG predictor with /Columns 4294967295 |
| `fuzz/corpus/pdf/content-bomb.pdf` | `4d039c6bcef4edf2dffb50346683daf2de678a99c143412118ab6cf38ea9cdbe` | a Flate stream inflating to 64 MiB |
| `fuzz/corpus/pdf/font-bomb.pdf` | `980d136aef247f0b8bc526d67da16eb347c42436710dfdfe261f13998cfce62e` | a /FontFile2 inflating to 64 MiB |
| `fuzz/corpus/pdf/cmap-bomb.pdf` | `d7932459c1aa55b9b622b62eceac7e7ab90aa62c2077ad6a5cb9f9f025591f7d` | a /ToUnicode CMap inflating to 64 MiB |
| `fuzz/corpus/pdf/cmap-range.pdf` | `f454083c3a9aa3417c898de13801da27e66d57edbd86ff6193dddb7dbc07ecc4` | a bfrange from 00000000 to FFFFFFFF |
| `fuzz/corpus/pdf/cmap-nesting.pdf` | `a5bfa2a2ba9ca60108b22ce6a81096a318edf31e32c4dba659b6c789d5f89849` | 20,000 nested brackets in a CMap and a Type 1 program |
| `fuzz/corpus/pdf/object-streams.pdf` | `125d472a6297d97094538ce341ade0635dea5e3783f1cba631db60f039e7bd02` | 65 object streams |
| `fuzz/corpus/pdf/objstm-bomb.pdf` | `5d5c5ad3eae03079dd3e7147c8ce1553392abb3a09d14d448d53f07db032afd6` | an object stream inflating past max_objstm_bytes |
| `fuzz/corpus/pdf/forms-deep.pdf` | `94680b63e8e62dfa6a62a6133d4bd53dbf73bedf4712eefdb1a7dcdcbf4ed0a6` | 100 nested Form XObjects |
| `fuzz/corpus/pdf/forms-fanout.pdf` | `0e68516fe4cd718da6e8c22e0cd7bf0eabd75173005a464122107197ea83e854` | four levels of 100 Form calls each |
| `fuzz/corpus/pdf/encrypted.pdf` | `1670c032a644a38a255f58923288bb6a19b1ef9490140bfafa86889a100b32ed` | an /Encrypt entry spelled /Encr#79pt |
| `fuzz/corpus/pdf/xref-stream.pdf` | `62f0b97efb778f4d745826f2a8edca2b5fbdeaeb12a047b9bfe2b908bdbb24d7` | an object stream and an xref stream |
| `fuzz/corpus/pdf/groups-and-clips.pdf` | `809a21be896d67eb0615aced80e98c9612f7ce79246b1a71f0eba6bf54f460bd` | a knockout group, blend arrays, optional content, a clip and an inline image |
| `fuzz/corpus/pdf/every-stream.pdf` | `2e2a535ce4c9c56f1ca396704e6e482fc147bc2e3b6e976e004f8f43dced45c9` | Flate content, a Form, a Type 1 program, a CMap, an ICC profile, an Indexed table and a sampled function |
