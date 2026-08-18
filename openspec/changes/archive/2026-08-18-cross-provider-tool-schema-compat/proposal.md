## Why

Live verification (OpenAI `strict:true`, Anthropic `strict:true`, Gemini's raw API, and the actual installed OpenCode client — all hit directly with Muninn's real generated schemas) confirmed most of the design but surfaced two more real, narrow problems beyond the two already fixed:

- **Anthropic `strict:true` rejects `minimum`/`maximum` on integer fields** (`"For 'integer' type, property 'minimum' is not supported"`), which every `limit`/`offset` field carries. A follow-up isolation test confirmed `minLength` (used on every scope field, fleet-wide) is fine — only the numeric range keywords are the problem. OpenAI's `strict:true` has no issue with either.
- **The installed OpenCode client's Gemini path — its `parameters` (legacy `Schema`) conversion, not `parameters_json_schema` — mangles nullable *array* fields specifically**, producing a real 400 from Google's API (`properties[updates].any_of[0].items: missing field`). Isolated against Gemini's raw API directly (`parameters_json_schema`, bypassing OpenCode) with the same nullable-array shape: accepted cleanly, and OpenAI/Anthropic strict mode both accept nullable arrays fine too. So this is specific to that one client's legacy-schema conversion, not a Gemini or strict-mode limitation — but it's exactly the client this deployment runs behind, so it's worth working around where the fix is cheap. It hits exactly the two `Option<Vec<T>>` fields in the whole fleet: `evolve_core_persona.updates` and `recall_memory_notes.filters`.

Investigating the second finding surfaced a better design for `evolve_core_persona` than the one shipped in the prior revision. That revision fixed the enum/`null` bug by dropping the single form's `which` from a schema `enum` to a plain nullable string — but the actual source of the awkwardness was having *two* argument forms (single + batch) in the first place. Collapsing to *only* the batch form — `updates`, always required, 1–5 entries, never optional — removes the nullable nested-form problem entirely: `which` inside each entry was already always-required and enum-typed with no `null` risk, so it can go back to being a real 5-value `enum` (better than the string-only fallback), and `updates` itself stops being a nullable array (fixing the live OpenCode/Gemini breakage as a side effect, for free, for this tool).

## What Changes

- Strip `minimum`/`maximum` (and `multipleOf`, defensively) from every integer/number field, fleet-wide — same treatment already given to `format:"uint64"`.
- **BREAKING** `evolve_core_persona`'s calling convention is consolidated to a single form: the top-level `which`/`content` fields are removed entirely; every call — including what used to be the "single form" — goes through `updates`, a required array of 1–5 `{ which, content }` entries with no duplicate `which`. `which` inside each entry reverts to a real, always-required, null-free `enum` (undoing the prior revision's `Option<String>` workaround, which is no longer needed once there's no top-level optional `which` left to protect). The response shape unifies to always be `{ results: [...] }` (previously the single form returned bare `{ bytes_written }`).
- **BREAKING** `recall_memory_notes.filters` drops its `Option` wrapper: it becomes a required array argument where an empty array (`[]`) means "no property filters," instead of an absent/`null` field. The handler already treats `None`, `Some(Value::Null)`, and an empty array identically, so this needs no handler-logic change — only the schema and call shape change.
- Regenerate `tests/snapshots/schema_snapshots__*.snap` and update every test that constructs the old `evolve_core_persona` single-form call shape or an absent/`null` `filters` argument.

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
- `mcp-server`: the schema-shape requirement gains a scenario that no tool schema declares `minimum`/`maximum`/`multipleOf` on a numeric field, fleet-wide.
- `memory-tools`: `evolve_core_persona`'s requirement is rewritten for the single-form-removed, always-`updates` calling convention and the unified `{ results }` response shape.
- `recall-search`: the `recall_memory_notes` tool requirement is modified so `filters` is documented as a required argument (empty array when unused) rather than an optional one, while its logical meaning ("supplying no property filters") is unchanged.

## Impact

- `src/tools.rs`: extend the `format:"uint64"`-style strip to also drop `minimum`/`maximum`/`multipleOf`; remove `EvolveFields`'s top-level `which`/`content` fields entirely (the prior revision's `Option<String>` retype of `which` is moot once the field itself is gone — `EvolveUpdateEntry.which` was never touched and stays a real `Which` enum); collapse `evolve_core_persona`'s handler to always process `updates` (delete the `has_single`/`has_batch` branching and the null-aware discriminator it needed); retype `RecallFields.filters` from `Option<Vec<PropertyFilterField>>` to `Vec<PropertyFilterField>`.
- `tests/`: update every `evolve_core_persona` test to the always-`updates` call shape and the unified response shape; update `recall_memory_notes` tests that omit or null `filters`; regenerate `tests/snapshots/schema_snapshots__*.snap`.
- No other files. Both `evolve_core_persona`'s single-form callers and `recall_memory_notes` callers that omit `filters` must update — this proposal does not preserve the old wire shapes for either.
