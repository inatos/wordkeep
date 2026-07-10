# Third-party notices

wordkeep is licensed under the MIT License (see [LICENSE](LICENSE)).

This project bundles or depends on the following notable third-party components.

## Rust crates (via Cargo)

See [Cargo.lock](Cargo.lock) for the full dependency graph. Major runtime dependencies include:

- [tree-sitter](https://github.com/tree-sitter/tree-sitter) and language grammars (`tree-sitter-cpp`, `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-c-sharp`, `tree-sitter-glsl`)
- [serde_json](https://github.com/serde-rs/json)
- [walkdir](https://github.com/BurntSushi/walkdir)

Optional feature dependencies:

- [fastembed](https://github.com/Anush008/fastembed-rs) (`embeddings` feature, ONNX Runtime model download on first use)
- [ratatui](https://github.com/ratatui-org/ratatui) (`dashboard` feature)

## Vendored Daslang grammar

- Upstream: [GaijinEntertainment/daScript](https://github.com/GaijinEntertainment/daScript)
- Location: `vendor/tree-sitter-daslang/`
- License: BSD 3-Clause (see `vendor/tree-sitter-daslang/LICENSE` and `vendor/tree-sitter-daslang/SOURCE.txt`)
- Built only when the `daslang` Cargo feature is enabled
