# Why I built wordkeep

Coding agents are only as good as the context they see. The default workflow is
to grep, open whole files, and paste hunks into chat. That works on small repos.
On large polyglot trees it burns tokens fast and still misses the one call site
that matters.

wordkeep is the MCP server I wrote to move that read off the model. It is a
local binary that parses source with tree-sitter, walks call graphs, searches
project docs, and returns token-budgeted answers over stdio. No network in the
default build. Point it at any repository with `--root`.

## The problem is context, not prompts

A single "where is this called?" question can touch dozens of files across
modules you did not open. An agent that reads hundreds of kilobytes of source
might spend tens of thousands of tokens and still truncate before it sees the
path you care about.

The fix is not a longer system prompt. It is tooling that already knows how to
classify definitions vs calls and return only what fits the budget.

## Measure what you displace

wordkeep tracks two numbers on every tool call:

- **distilled**: roughly how many tokens it would take to read the raw material
- **returned**: what the tool actually emitted

The `stats` tool and optional dashboard make that leverage visible. If a tool
starts returning more than it distills, something is wrong with the budget or
the extractor.

That mindset matches how I work on performance-sensitive code: measure the hot
path, cache what does not change, and prefer exact parsers over guessing.

## What it helps with

**Call graph before a signature change.** `call_graph` gives one hop of callers
and callees with file:line sites. Enough to scope a refactor without opening
every hunk in a diff.

**Trace hitch hunts.** `trace_summary` with `sort_by: max` surfaces the worst
zones. `trace_profile` bundles that with git blast radius and index freshness
so you are not manually chaining three tools mid-debug.

**Design notes without loading every rule.** `knowledge_search` returns the few
relevant chunks for a query instead of always-loading the whole rules tree.

**One call instead of four.** `symbol_context` stitches definition body,
callers, callees, and (for records) field layout into one bounded payload.

## Try it

wordkeep is project-agnostic by design. Clone the repo, `cargo build --release`,
point your MCP client at the binary with `--root`, and run the
[manual smoke test](README.md#manual-smoke-test) against wordkeep's own source
tree.

If you wire agents against a large C++ or polyglot repo, run `stats` for a week.
The numbers are more convincing than any README claim.

For setup details see [README.md](README.md) and [docs/onboarding.md](docs/onboarding.md).
