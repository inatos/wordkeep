# SemIf semantic decisions

Host-agnostic MCP tool for runtime-defined semantic ifs: unstructured `state` +
`question` + typed `options` → option probabilities without decoding an answer
sentence. Inspired by [SemIf / OpenJev](https://github.com/TheoLeeCJ/SemIf-OpenJev)
(independent; not affiliated with Jev or TypeSafe).

## Tool

`semantic_decide` — see [tools.md](tools.md).

## Config

```json
{ "semif": { "enabled": true, "backend": "heuristic" } }
```

Unknown `backend` values fall back to the heuristic scorer and set `fallback: true`.

## Knowledge hook

Pass `semif:true` on `knowledge_search` / `knowledge_answer` to rerank the BM25
head with the same scorer. Distinct from embedding `semantic` (requires
`--features embeddings`).

## Fixtures

`tests/fixtures/semif/decisions.jsonl` — owned light-audit rows (no model weights).

## Out of scope (v1) / follow-ups

Torch sidecar, llama.cpp GGUF, temperature calibration, WebGPU demo.

**Candidate:** a Rust port of SemIf’s direct option-logit scoring (keep the
`Scorer` trait; heuristic remains the offline default). Pick backend
(GGUF/llama.cpp vs torch sidecar vs native Rust) before expanding scope.
