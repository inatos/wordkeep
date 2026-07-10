# Contributing

Thanks for helping improve wordkeep.

## Development setup

```sh
git clone https://github.com/inatos/wordkeep.git
cd wordkeep
cargo test
cargo build --release
```

Optional features:

```sh
cargo test --features daslang
cargo test --features dashboard
cargo test --features embeddings
```

## Pull requests

1. Open an issue for large changes when possible.
2. Keep diffs focused. Match existing style in touched files.
3. Run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo test` before opening a PR.
4. Add or update tests when behavior changes.

## MCP contract

New tools should follow the existing pattern:

- exact tree-sitter extraction where a grammar exists
- token budgets on every response
- honest `stats` distilled/returned accounting
- paths validated through `config::paths_from_args`

Register the tool in `src/main.rs` and add an integration test in `tests/mcp_stdio.rs`.

## Security

See [SECURITY.md](SECURITY.md).
