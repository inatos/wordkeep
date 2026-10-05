# SemIf semantic decisions

Host-agnostic MCP tool for runtime-defined semantic ifs: unstructured `state` +
`question` + typed `options` → option probabilities without decoding an answer
sentence. Inspired by [SemIf / OpenJev](https://github.com/TheoLeeCJ/SemIf-OpenJev)
(independent; not affiliated with Jev or TypeSafe). Model-backed scoring uses
[Ereshkigal](https://github.com/inatos/ereshkigal) (`direct-options-v1` letter logits).

## Tool

`semantic_decide` — see [tools.md](tools.md).
`decree` — `check`/`lint`/`run`/`test`/`decide`/`stats` over an Ereshkigal library
(`semif.decrees`, default `tools/ereshkigal/decrees`). Check/lint are in-process;
run/test/decide/stats use a resident `ereshkigal serve --stdio` child.

## Config

```json
{
  "semif": {
    "enabled": true,
    "backend": "ereshkigal",
    "mode": "shared",
    "debias": "adaptive",
    "tandem": true,
    "adaptive": { "qhat": 0.1, "margin_min": 0.15 },
    "knowledge_debias": "none",
    "n_seq_max": 32,
    "adapter": ".wordkeep/models/qwen3-0.6b-lora-dev.gguf",
    "adapter_scale": 1.5,
    "temperature": 1.0,
    "gguf": ".wordkeep/models/Qwen3-0.6B-Q8_0.gguf",
    "gguf_verify": ".wordkeep/models/Qwen3.5-4B-Q4_K_M.gguf",
    "cascade": { "routing": "conformal", "qhat": 0.1 },
    "n_gpu_layers": 99,
    "tokenizer_source": "Qwen/Qwen3-0.6B",
    "tokenizer_revision": "c1899de289a04d12100db370d81485cdf75e47ca",
    "decrees": "tools/ereshkigal/decrees"
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
- **Debias:** `adaptive` (default) runs identity first and only cyclic-permutes when
  the conformal set is not a singleton **and** top1−top2 &lt; `adaptive.margin_min`.
  Keep `none` / `permute` / `pride` as explicit overrides. Retrieval
  `semif:true` stays on `knowledge_debias: none` (one forward; 5 clipped
  candidates).
- **Tandem:** when `tandem: true` and GPU layers are live, Wordkeep keeps a second
  draft engine at `n_gpu_layers=0`. Cascade order:
  Vulkan draft → adaptive → conformal singleton → CPU draft escalate → 4B verify.
  `cascade_source`: `cascade-draft` | `cascade-adaptive` | `cascade-cpu` |
  `cascade-verify` | `cascade-skipped`.
- **Lazy verify:** prefetch / `ensure_loaded` warms **draft (+ tandem CPU draft)
  only**. The 4B `gguf_verify` loads on the first escalate that needs it
  (`gguf_verify_loaded: false` until then; `gguf_verify_lazy: true` when pinned).
  First `cascade-verify` pays cold 4B load; repeats stay warm. On a 1080 Ti, keep
  **at most one** Wordkeep MCP (wiki does not hold GGUF) or verify OOMs →
  `cascade-skipped`. Helper: `tools/ereshkigal/scripts/tandem_verify_bench.sh`.
- **Adapter:** `semif.adapter` / `ERESHKIGAL_ADAPTER` attaches LoRA on draft (+ CPU
  draft) only — verify stays bare. `adapter_scale` (env `ERESHKIGAL_ADAPTER_SCALE`,
  default 1.0; pinned **1.5** after holdout sweep) is runtime-only.
- **Temperature:** `semif.temperature` divides option logits before softmax
  (argmax-invariant). Fit via grouped OOF on train; when OOF collapses to the
  0.05 floor, keep **1.0** and report ECE@T1. `calibrated: true` only when T≠1.
- **Batch wavefront:** `semantic_decide` `{state, batch:[…]}` draft-scores all
  rows (`score_batch` / shared suffixes when `mode: shared`), applies debias,
  compacts conformal residuals, then CPU → verify waves before scattering.
- **`n_seq_max`:** production pin **32** (config clamp ceiling **64** — older builds
  silently capped at 8). Shared vs direct adapter microbench:
  `tools/ereshkigal/scripts/adapter_shared_bench.sh`. Status JSON exposes
  `n_seq_max` / `adapter_scale` / `temperature`. Larger `n_seq_max` can hit
  `NoKvCacheSlot` on knowledge rerank; retrieval falls back to BM25.
- llama.cpp allows one backend init per process. `ereshkigal-core` shares a
  process-global leaked `LlamaBackend` so tandem CPU draft and `gguf_verify` can
  load after the GPU draft (without this, the second load fails
  `BackendAlreadyInitialized`).
- `n_gpu_layers` is attempted then CPU (`0`) if Vulkan/GPU init fails.
- Paths are gitignored; override with `ERESHKIGAL_GGUF` / `ERESHKIGAL_GGUF_VERIFY`
  / `ERESHKIGAL_ADAPTER` / `ERESHKIGAL_ADAPTER_SCALE`.

## Decree binary resolve

`ERESHKIGAL_BIN` / `semif.serve_bin` →
`tools/ereshkigal/target-vulkan/release/ereshkigal` when Wordkeep is built with
`ereshkigal-vulkan` and that binary exists → else `target/release` →
`target/debug` → PATH `ereshkigal`. Missing binary fails soft with stderr on
`run`/`test`/`stats`.

## Dual A/B gates

| Gate | Script | Rule |
| --- | --- | --- |
| CPU SoT | `tools/ereshkigal/scripts/ab_cpu_1e-6.sh` | CPU ggml vs Python SemIf @ **1e-6** |
| Vulkan smoke | `tools/ereshkigal/scripts/ab_vulkan_smoke.sh` | argmax + 64-char prompt SHA (loose Δp); **never** 1e-6 |

## Knowledge hook

Pass `semif:true` on `knowledge_search` / `knowledge_answer` to rerank the BM25
head (`knowledge_debias: none` by default: one forward, 5×180-char candidates).
Distinct from embedding `semantic` (requires `--features embeddings`).

## Dashboard

Wiki **Dashboard → Ereshkigal** (`?tab=dashboard&view=ereshkigal`) is always in
the Dashboard tablist. Status should show `cold_load_ms`, `n_gpu_layers_used`,
`cpu_draft_loaded`, `tandem`, and a `cascade_histogram`. Recent-calls **Wall Time**
shows `round(timing_us/1000)` with an `ms` suffix (wire format stays `timing_us`).
Bakeoff captions are Vulkan-aware (CPU 1e-6 vs Vulkan smoke). MCP start
**prefetches** GGUF so the first agent decide is warm. `write_status` read-merges
prior extras, so `cpu_draft_loaded` / `cold_load_ms` survive empty resolve
refreshes. Engines are still proven by `cascade_source` on live decides.

## Measured latency (GTX 1080 Ti, 2026-10-05)

Live MCP after prefetch + adaptive + shared backend. Do not mix with CPU 1e-6
A/B rows or sandbox permute p50 (those can be CPU fallback). **Lazy verify**
(2026-10-05 later): prefetch warms draft+tandem only; first `cascade-verify`
pays cold 4B load. Sole MCP + ≥~2.5 GiB free VRAM required or verify OOMs →
`cascade-skipped`. Helper: `tools/ereshkigal/scripts/tandem_verify_bench.sh`.

| Probe | cascade | timing |
| --- | --- | ---: |
| Cold MCP (load + first) | cascade-draft | ~2365 ms |
| Sharp, post-prefetch | cascade-draft | 111–168 ms |
| Sharp, KV-warm repeat | cascade-draft | **3.8 ms** |
| Escalate→4B, first (lazy load) | cascade-verify | **~11978 ms** |
| Escalate→4B, warm repeat | cascade-verify | **12.2 ms** |

Escalate cost is front-loaded: quality on unsure rows, cheap repeats. Prefetch
`cold_load_ms` ≈6 s covers draft+tandem only (`gguf_verify_loaded: false` until
first escalate). Full tables:
[.wordkeep/notes/ereshkigal-semif-benchmarks.md](../../.wordkeep/notes/ereshkigal-semif-benchmarks.md).
JSON: `tools/ereshkigal/results/quality/tandem_verify_bench.json`.

## Quality track (dev gold + LoRA adapter)

Labeled folds live in `tools/ereshkigal/fixtures/dev_gold.jsonl` (**not**
authored144; n≥30, currently 52). Optimizer: `scripts/optimize_dev_gold.sh`.
LoRA: `scripts/lora_dev_gold.sh` — strict split, SemIf prompts, BA/ECE gate
**0.75** before convert. Gated adapter GGUF:
`.wordkeep/models/qwen3-0.6b-lora-dev.gguf` via `scripts/convert_lora_gguf.sh`.

Draft load honors `semif.adapter` / `ERESHKIGAL_ADAPTER` (scale 1.0); verify stays
bare. `semif-score --adapter` for offline parity. Latest gated holdout:
student BA **1.0** / ECE 0.099; PEFT↔GGUF argmax agreement **100%**. Do not claim
1e-6.

## Fixtures

`tests/fixtures/semif/decisions.jsonl` — owned light-audit rows.
`heuristic-baseline-decisions.jsonl` / `heuristic-baseline-knowledge.json` —
heuristic before cut. After GGUF, compare with `SEMIF_WRITE_AFTER=1` /
`ERESHKIGAL_GGUF`.

## Local Betwixt

Launchers (`mcp.sh`, `wiki.sh`, `tools/dev/rebuild_wordkeep.sh`) build
`--features embeddings,daslang,dashboard,ereshkigal-vulkan` so llama.cpp offloads
`n_gpu_layers` on the NVIDIA ICD (`VK_ICD_FILENAMES`). CPU-only:
`WORDKEEP_FEATURES=embeddings,daslang,dashboard,ereshkigal`. Vulkan cmake prefers
**in-tree** `tools/ereshkigal/vendor/{vulkan-headers,spirv-prefix}` (populate with
`scripts/vendor_vulkan_headers.sh`) then sibling / distro headers, and wipes a
CPU `llama-cpp-sys-2` build dir first. Pin GGUF under `.wordkeep/models/`
(gitignored); a symlink to a local Ereshkigal `models/Qwen3-0.6B-Q8_0.gguf` is enough.

CI with `--features ereshkigal` may cache the 0.6B Q8 smoke GGUF. Do not put 4B
Qwen3.5 in a 1e-6 probability gate.
