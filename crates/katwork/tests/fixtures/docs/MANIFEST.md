# Document reader fixtures

Every committed file the document readers are tested or fuzzed with is listed here with its
SHA-256 and what made it. Hostile packages (zip bombs, lying directories, encrypted entries,
DOCTYPEs, deep nesting) are never committed: `tests/docs_read.rs` generates each one inside the
test that uses it, from `tests/support/ooxml.rs`.

A file made in Word, with Typst or with `wordcraft-cli` may be added under this directory, with a
row naming the tool and its version.

## Fuzz seeds (`fuzz/corpus/`)

Made by `cargo test -p katwork --test docs_read -- --ignored --exact regenerate_fuzz_seeds`, which
rewrites the corpus and this table from `tests/support/ooxml.rs`. `fuzz_seeds_match_the_manifest`
checks that the committed seeds are exactly the files listed, with these hashes.

| Seed | SHA-256 | What |
|---|---|---|
| `package/docx-deflated.zip` | `9e461a14b1f69589258c3c3d7a320022defd47fe9010ac69e4bdd3ebad27d969` | a one-paragraph DOCX, deflated |
| `package/docx-stored.zip` | `728dc5ff8b22d69e113037dd69192d230b9c83a6b756497b543228533d4c2da7` | the same package through to_stored_zip |
| `package/empty.zip` | `8739c76e681f900923b900c9df0ef75cf421d39cabb54650c4b9ad19b6a76d85` | an empty archive: one end record |
| `package/zip64-empty.zip` | `1386d0a55e7ede0dd147c146d17edfa81720aa3a50171ba3cc6b774839dc9593` | an empty archive with ZIP64 end records |
| `package/data-descriptor.zip` | `09b62d9b9b03ab6751de69b05021900d2b3f5e9ddfe3e8f2b4b0ce019c94e0e3` | an entry with a data descriptor |
| `package/two-end-records.zip` | `e9e8c13e08cf0cd0ac83027102ffcb99b47fd102bd652dcbe65820e70b5db3cd` | a second end record before the real one |
| `package/duplicate-names.zip` | `5a89dc07bcdd7d669e9be542bdbd351960688c66e7d1c855537c6cef1ce93c7a` | two names equal under ASCII case folding |
| `package/encrypted-flag.zip` | `1eb183da5858ea67248999d13fceb6371aec2ff868ea36850bf9719c7eab4a60` | an entry with the encryption flag |
| `docx/tab-break.docx` | `51d5a3851d7857931261918f6f3e8342ccbdac9a52568bede968b11ef2e05abe` | tabs and breaks in a run |
| `docx/table-styles.docx` | `4a3323c384e53d0075ca45572f1021ff3ac841e36edee2af004b1f42f1f494b4` | a table style, regions, band size and pct shading |
| `docx/tracked.docx` | `4cc529d2725d3fd08ec625fd6aa1cbe03037246810abbc24298c4d71b98a6304` | insertions, deletions, moves and a field |
| `docx/hidden-runs.docx` | `dbf55d840673fba619137e42b5944b0fd2d9bb045a473b999a0633e09473df25` | vanished, white, tiny and highlighted runs |
| `docx/styles.docx` | `e4c59326ed12a79f86f5052efb3d94802092ae9afa372cc372a301c5b2cc2b27` | default, based-on, character and paragraph styles |
| `docx/theme.docx` | `6b055c273eb8d9864fb924f8e3c76a8235c495ec628db70fdc3107c8c94a7a64` | a theme, its scheme mapping, a page colour and themed shading |
| `docx/alternate-content.docx` | `ef522df7b2547d2f85b352fbad71c7b5420bd8a7ad46cc3acbbbdf9c56c6aa9b` | a text box in mc:Choice and mc:Fallback |
| `docx/entities.docx` | `a5309df9e78c6fea173572a491a2a02e9814895e564c34c90e36ab03506a0413` | entity and character references |
| `docx/notes.docx` | `5541c4c15c536ac3c6fbb4ef69888a8e80c4317fea47a2a7f369aa1a4970c1b3` | a header, a footer, footnotes and endnotes |
| `docx/strict.docx` | `8a23951a689ec0c902730a14a06f21274225f872f243ea4585154697b941c643` | a Strict OOXML main part |
| `text/thai.txt` | `ef063020a7aea5b6697c343a6ed2316ae9c3fefdc30081fba213b37fce7ed374` | Thai with a zero-width space and SARA AM as U+0E4D U+0E32 |
| `text/crlf.txt` | `d9e18041262397cdbc90e4bf29cf5d820ca9427dc36f9ba6f1d4b8ed65477a86` | CRLF line ends and a blank line |
| `text/utf16le.txt` | `a4bddd776d8907aab7022cd6cf68a4a3f462e155f9727779671df516c3f14f45` | UTF-16 LE with a byte-order mark |
| `text/utf16be.txt` | `8bd16e52f79679ed4b45558257fb2ee8c9eac5e699df6bd57102755e309c9af0` | UTF-16 BE with a byte-order mark |
| `text/bom-only.txt` | `f1945cd6c19e56b3c1c78943ef5ec18116907a4ca1efc40a57d48ab1db7adfc5` | a UTF-8 byte-order mark alone |
| `text/odd-utf16.txt` | `c95a4fead577ca197f1094206aeb3263c8b553f5c4e89a00354f1db980c49657` | UTF-16 with an odd byte count |
| `text/lone-surrogate.txt` | `7dede2c66266f92ce35bf21a950f202c75633079eca964654959a3c611acaaa6` | UTF-16 with a lone surrogate |
| `text/nul.txt` | `59b271ae1bbcb1d31d41929817f4b16fb439eb4f31520b5ad1d5ce98920a7138` | a NUL character |
