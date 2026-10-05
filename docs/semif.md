# SemIf semantic decisions

Host-agnostic MCP tool for runtime-defined semantic ifs: unstructured `state` +
`question` + typed `options` → option probabilities without decoding an answer
sentence. Inspired by [SemIf / OpenJev](https://github.com/TheoLeeCJ/SemIf-OpenJev)
(independent; not affiliated with Jev or TypeSafe). Model-backed scoring uses
[Ereshkigal](https://github.com/inatos/ereshkigal) (`direct-options-v1` letter logits).

## Tool

`semantic_decide` — see [tools.md](tools.md).

## Config

```json
{
  "semif": {
    "enabled": true,
    "backend": "ereshkigal",
    "mode": "shared",
    "debias": "permute",
    "knowledge_debias": "none",
    "n_seq_max": 1,
    "gguf": ".wordkeep/models/Qwen3-0.6B-Q8_0.gguf",
    "gguf_verify": ".wordkeep/models/Qwen3.5-4B-Q4_K_M.gguf",
    "cascade": { "routing": "conformal", "qhat": 0.1 },
    "n_gpu_layers": 99,
    "tokenizer_source": "Qwen/Qwen3-0.6B",
    "tokenizer_revision": "c1899de289a04d12100db370d81485cdf75e47ca"
  }
}
```

- **Without** `--features ereshkigal`, the crate default backend is `heuristic`.
- **With** the feature, the default backend is `ereshkigal`. Missing GGUF is an
  error on `semantic_decide` (not a silent heuristic score). `knowledge_search`
  `semif:true` keeps BM25 order and records a fallback event.
- `backend: heuristic` remains for air-gapped tests. Unknown names (e.g. `torch`)
  still fall back to the heuristic with `fallback: true`.
- Heuristic `prompt_sha256` hashes the STATE/QUESTION/OPTIONS recipe. GGUF hashes
  `direct-options-v1`. Do not compare hashes across backends.
- Permute remaps with `(i+k)%n` on `semantic_decide` (`debias`). Retrieval
  `semif:true` defaults to `knowledge_debias: none` (one forward; 5 clipped
  candidates) so GGUF KV fits. Set `knowledge_debias: permute` to match decide.
  Cascade is
  conformal singleton-commit on `semantic_decide`; set `knowledge_cascade: true`
  to use 4B on search (off by default).
- `n_gpu_layers` is attempted then CPU (`0`) if Vulkan/GPU init fails.
- Paths are gitignored; override with `ERESHKIGAL_GGUF` / `ERESHKIGAL_GGUF_VERIFY`.

## Knowledge hook

Pass `semif:true` on `knowledge_search` / `knowledge_answer` to rerank the BM25
head (`knowledge_debias: none` by default: one forward, 5×180-char candidates).
Distinct from embedding `semantic` (requires `--features embeddings`).

## Dashboard

Wiki **Dashboard → Ereshkigal** (`?tab=dashboard&view=ereshkigal`) is always in
the Dashboard tablist. Bakeoff rows are **this GTX 1080 Ti** same-GGUF A/B
(0.6B 144/144 @ 1e-6; bartowski 4B 3/3 @ 1e-2; Wordkeep 7-row p50 575 ms none /
2176 ms permute). Published SemIf BF16 is a different checkpoint, not a latency
table.

## Fixtures

`tests/fixtures/semif/decisions.jsonl` — owned light-audit rows.
`heuristic-baseline-decisions.jsonl` / `heuristic-baseline-knowledge.json` —
heuristic before cut. After GGUF, compare with `SEMIF_WRITE_AFTER=1` /
`ERESHKIGAL_GGUF`.

## Local Betwixt

Launchers (`mcp.sh`, `wiki.sh`, `tools/dev/rebuild_wordkeep.sh`) build
`--features embeddings,daslang,dashboard,ereshkigal` (and `ereshkigal-vulkan`
when an ICD is present). Pin GGUF under `.wordkeep/models/` (gitignored); a
symlink to a local Ereshkigal `models/Qwen3-0.6B-Q8_0.gguf` is enough.

CI with `--features ereshkigal` may cache the 0.6B Q8 smoke GGUF. Do not put 4B
Qwen3.5 in a 1e-6 probability gate.
