# Onboarding: MCP servers and what wordkeep does

This guide is for someone who has never built or used a **Model Context Protocol
(MCP)** server. It explains what MCP is, how a server like **wordkeep** plugs into an
AI coding agent, and the specific ideas wordkeep contributes. If you just want to wire
it up and go, the top-level [README](../README.md) has the quick steps; come back here
when you want the *why*.

---

## 1. The problem: context is expensive

An LLM coding agent can only "see" what you put in its context window. The naive way to
answer "where is `foo` called?" or "what does this module look like?" is to dump whole
files into the prompt. That is slow, blows the token budget, and buries the few
relevant lines under thousands of irrelevant ones. The agent then pays (in latency and
money) to re-read material it mostly ignores.

The better approach is **distillation**: a tool that reads the raw material *locally*
and hands the agent only the high-signal answer - a symbol map, a call graph, the three
relevant doc chunks. wordkeep is a collection of such tools.

---

## 2. What MCP actually is

MCP is an open protocol that standardizes how an AI application (the **host/client**  - 
VS Code Copilot, Cursor, Claude Desktop, Zed, …) talks to an external **server** that
provides *tools*, *resources*, or *prompts*. Think of it as "language-server protocol,
but for giving models capabilities."

The pieces you need to know:

- **Client / server.** The editor is the client. wordkeep is a server. The client
  launches the server as a subprocess and they speak a defined protocol over a pipe.
- **Transport.** MCP supports a couple of transports; the simplest is **stdio**: the
  client writes requests to the server's standard input and reads responses from its
  standard output. No network, no ports, no auth to configure. wordkeep uses stdio.
- **Messages: JSON-RPC 2.0.** Every message is a JSON object. A request has an `id`, a
  `method` (e.g. `tools/call`), and `params`. The server replies with a matching `id`
  and either a `result` or an `error`. wordkeep uses **newline-delimited** JSON-RPC:
  one JSON object per line.
- **The handshake.** The client sends `initialize`; the server replies with its name,
  version, and capabilities. After that the client calls `tools/list` to discover what
  the server offers, and `tools/call` to invoke a specific tool with arguments.

A minimal session looks like this (client lines in, server lines out):

```jsonc
// client → server
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
// server → client: "I am wordkeep, here are my capabilities"
{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"wordkeep", ...}, ...}}

// client → server
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
// server → client: the catalog of tools + their JSON input schemas
{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"repo_map", ...}, ...]}}

// client → server: run one tool
{"jsonrpc":"2.0","id":3,"method":"tools/call",
 "params":{"name":"outline","arguments":{"file":"src/config.rs"}}}
// server → client: the distilled answer as text content
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"outline - …"}]}}
```

Each tool advertises a **JSON Schema** for its arguments, so the client (and the model)
knows what inputs are valid. That schema is what shows up when an agent decides which
tool to call and how.

---

## 3. How wordkeep implements a server

wordkeep is deliberately small and boring where it can be, so it is fast and easy to
trust:

- **Single static binary, instant cold start.** No async runtime, no web framework - it
  reads stdin line by line, parses the JSON-RPC, dispatches to a handler, and writes one
  JSON line back. The whole loop is a few hundred lines.
- **`--root <path>`.** The one thing it needs is which repository to operate on. Every
  tool resolves paths relative to that root, which is what makes it **project-agnostic**:
  the same binary serves any repo. Optional **`WORDKEEP_ROOT`** env var is a fallback
  when `--root` is omitted.
- **`.wordkeep/config.json`.** When a tool's `paths` argument is omitted, wordkeep reads
  `default_paths` from this file (fallback `["src"]`). Optional `test_command` feeds
  `test_map` filter hints and pitfall verify lines. Copy
  [`.wordkeep/config.example.json`](../.wordkeep/config.example.json) to your repo root
  as a starting point.
- **Stateless requests, cached work.** Each `tools/call` is independent, but expensive
  parsing is memoized (see §6) so repeated calls are cheap.

If you read the source, `main.rs` builds a table of tools (name, description, input
schema, handler closure) and `mcp.rs` runs the stdio/JSON-RPC loop against that table.
Adding a tool is "write a module with a `build(root, args) -> Result<String, String>`
and register it" - nothing else in the protocol layer changes.

---

## 4. The core idea: distilled vs returned

Every wordkeep tool reads some raw material and emits a much smaller answer. It tracks
both numbers:

- **distilled** - an estimate of the tokens it would have cost the agent to read the raw
  material itself (the files it parsed, the trace it digested).
- **returned** - the tokens it actually emitted.

The `stats` tool prints the running ratio. In practice a `symbol_refs` query over a
large module routinely turns tens of thousands of "tokens you'd otherwise read" into a
couple hundred returned - a 99%+ reduction. Making that leverage *measurable* is part of
the point: it turns "trust me, this helps" into a number you can watch. The
[dashboard](../README.md#dashboard) (`cargo run --features dashboard -- dashboard`)
renders the same counters as a live terminal UI - per-tool savings, single-call
watermarks, a rolling activity log, and a health panel - refreshing once a second.

Two design rules follow from this:

1. **Token budgets everywhere.** Tools take a `token_budget` and stop emitting when they
   hit it, with a "+N more" marker, so a single call can never balloon the context.
2. **Exactness over guessing.** The distilled answer is only useful if it is correct,
   which leads to the next idea.

---

## 5. Exact extraction with tree-sitter (not grep)

Most "find the symbol" tooling is regex/grep under the hood, which can't tell a
definition from a comment from a substring of an unrelated word. wordkeep parses real
**syntax trees** with [tree-sitter](https://tree-sitter.github.io/) and classifies each
occurrence by its node kind:

- a class/struct/function **definition**,
- a **call** site,
- some other **reference**.

That means `symbol_refs` ignores the `// foo()` in a comment and the `foo` inside
`foobar`, resolves qualified names like `A::b`, and recognizes out-of-line member
definitions (`void Foo::Bar() {}`). The same parsed trees power `repo_map` (structure),
`outline` (one file's symbols with line numbers), `call_graph` (one hop of callers and
callees), and `type_layout` (a record's fields). Exactness is the feature.

---

## 6. What wordkeep innovates on

These are the parts worth copying if you build your own server.

**Two-tier, mtime-keyed caching.**
Parsing is the expensive step, so wordkeep never parses the same file twice unless it
changed. Each file's distilled product (symbols, occurrences, call edges, knowledge
chunks) is cached keyed by `path + mtime`. Some caches **persist to disk** (so even a
fresh process spawn over a big tree skips untouched files); the heavier per-file
occurrence lists live in an **in-memory** memo for the process lifetime. The first query
that touches a file pays the parse; every later query just filters cached data. Parsed
`tree-sitter` trees are also kept warm and **re-parsed incrementally** (`Tree::edit`)
when a file's bytes change, so editing a hot file mid-session avoids a full re-parse.

**Parallel cold parse.**
When a query does hit a batch of un-cached files, wordkeep fans the parses out across
threads (one parser per thread, bounded by available cores), then publishes the results
into the shared memo. Cold starts stay fast on large repos.

**Multi-language by construction.**
A file's extension picks the grammar; one code path serves C/C++, GLSL, Rust, Python,
C#, and TypeScript. Languages without their own grammar are handled gracefully - Daslang
falls back to a light line scanner (or exact tree-sitter when built with
`--features daslang`), and **Svelte** is parsed by blanking everything outside its
`<script>` blocks (preserving newlines, so line numbers stay accurate) and running the
TypeScript grammar over the remainder. Adding a language is mostly a grammar dependency
plus an extension mapping.

**Composability.**
New tools are built from existing extractors rather than new machinery. `outline` reuses
the `repo_map` symbol locator; `test_map` reuses the `symbol_refs` classifier scoped to
the test tree; `call_graph` shares `symbol_refs`' occurrence model; `doc_comment`,
`symbol_context`, `diff_map`, and `undocumented` share one `symbol_def` definition
locator; `symbol_context` literally stitches `call_graph` and `type_layout` together;
`call_path` runs a BFS over `call_graph`'s adjacency; `usage_examples` wraps the
`symbol_refs` call classifier with surrounding context lines; and `undocumented` pairs
the `symbol_def` doc-presence check with `call_graph` fan-in. `diff_map` even re-parses
each changed symbol's signature in the git blob to tell an ABI break from a body-only
edit. This keeps behavior consistent and the binary small.

**Deterministic, offline by default - smarter when asked.**
The default build has no network calls and produces identical output for identical
input (knowledge search is pure BM25). An optional `--features embeddings` build adds
semantic reranking via a local model, but BM25 remains the fallback, so the tool never
*depends* on a download to work.

**Honest telemetry.**
The `stats` savings counters aren't decoration - they're how you justify the tool and
spot a tool that's returning too much. The persisted schema (**v3**) keeps per-tool
single-call **watermarks**, a rolling **event log**, per-call latency, and outcome tags
(`ok`, `truncated`, `low_yield`, `error`); older v1/v2 files load with missing fields
defaulting to zero. The [dashboard](../README.md#dashboard) renders the same data live -
including improvement signals (slow tools, high truncation, never-called registry
entries) and a health panel that flags any **net-negative** tool (one returning more
than it distilled). Building measurement in from the start is the innovation as much as
any single feature.

---

## 7. The toolbox at a glance

Full per-tool descriptions live in the [README](../README.md#tools). This table is the
fast index:

| Tool | Answers |
| --- | --- |
| `repo_map` | What's the structure of this tree? (namespaces, types, signatures) |
| `outline` | What symbols are in this one file, and on what lines? |
| `symbol_refs` | Where is this symbol defined, called, and referenced? |
| `call_graph` | What does this function call, and who calls it? |
| `call_path` | What's the shortest call chain from one function to another? |
| `include_graph` | Who includes this header, and what does it include? |
| `type_layout` | What are this struct/class's fields, and are any non-POD? |
| `test_map` | Which tests exercise this symbol (so I run the narrowest suite)? |
| `doc_comment` | What's this symbol's doc comment and signature (intent + contract)? |
| `symbol_context` | Everything to edit this safely: body + callers/callees + layout? |
| `usage_examples` | How is this API actually used? (call sites with context lines) |
| `undocumented` | Which exported symbols lack docs, ranked by caller count? |
| `dead_code` | Which defined symbols have zero callers and zero references? |
| `big_functions` | Which functions are largest by line span (refactor candidates)? |
| `symbol_diff` | How did one symbol's definition change vs a git ref? |
| `module_map` | How are modules coupled (cross-module call edges)? |
| `diff_map` | Which symbols changed vs a git ref, and who calls them? |
| `knowledge_search` | Which docs/design notes are relevant to this question? |
| `knowledge_upsert` | Write or update a markdown section for future search |
| `trace_summary` | What are the hottest zones in this Tracy capture (and regressions)? |
| `trace_profile` | Hitch workflow: max-sorted trace + git blast radius + index freshness |
| `integration_hooks` | Curated cross-subsystem call sites (+ optional `call_path`) |
| `index_stale` | Are on-disk indexes (`call_graph.json`, `knowledge-chunks.json`) stale? |
| `stats` | How many tokens has wordkeep saved? |
| `mas_post` / `mas_read` / `mas_status` / `mas_finalize` | Recursive-MAS blackboard handoff between subagents |

---

## 8. Recursive MAS (optional)

For multi-subagent workflows (planner → solver → critic), wordkeep exposes a
token-capped session blackboard instead of re-pasting bulky chat between agents.
See the [README MAS section](../README.md#recursive-mas-multi-agent-blackboard) for the
typical loop and [recursive_mas.md](designs/recursive_mas.md) for design rationale.

---

## 9. Try it and extend it

Run the [manual smoke test](../README.md#manual-smoke-test) to watch real requests and
responses go by, then call `stats` to see the savings. To add your own tool, follow an
existing module (`outline.rs` is the smallest), register it in `main.rs`, give it a
JSON input schema, and keep the contract from §4–§6: exact extraction, a token budget,
deterministic defaults, and honest savings accounting. The README **Extend** section
lists optional future work if telemetry surfaces a need.
