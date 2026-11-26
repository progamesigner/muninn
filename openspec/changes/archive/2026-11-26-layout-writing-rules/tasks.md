# Tasks

## 1. Reproduce

- [x] 1.1 Reproduce the reported edit failure against production: six link forms match, a search ending inside a link (`Muninn]] ok1b`) fails with `edit_search_not_found`
- [x] 1.2 Check write size on the server: a 112 KB note round-trips byte for byte against a local server, so truncation is client-side
- [x] 1.3 Check `\"` in `replace_string`: the server stores the bytes it receives; the backslash comes from the caller

## 2. Edit on the clean view

- [x] 2.1 Record the span of every rewritten link in `rewrite_links` and add `edit_clean_view`, which widens a match to whole links and re-expands only that range
- [x] 2.2 Add `Storage::edit_with` and use it in `edit_memory_note` when the scheme is non-empty
- [x] 2.3 Verify tests for a search cutting through a link, bytes outside the edit kept, and ambiguity counted in the clean view

## 3. path_prefix forms

- [x] 3.1 Add `PathResolver::prefix_filter` and use it in `list_memory_notes` and `recall_memory_notes`
- [x] 3.2 Update both tool descriptions, refresh schema snapshots, and verify a test for `topics`, `topics/`, `Agents/topics`, and `Agents/topics/` in both tools

## 4. Layout and conventions

- [x] 4.1 Add "Links between notes" and the `path_prefix` sentence to the default layout without mentioning the suffix
- [x] 4.2 Add `MUNINN_MEMORY_CONVENTIONS_FILE` and `--memory-conventions-file`, wire them through `TemplatePaths`, and verify config tests for default, relative, and absolute paths
- [x] 4.3 Append the conventions file after the layout and verify tests for absent, blank, global, and per-scope files
- [x] 4.4 Document the variable, the conventions file, the edit behavior, and `path_prefix` in the README

## 5. Checks

- [x] 5.1 Run `cargo fmt --check`, `cargo clippy --all-targets`, and `cargo test`
- [x] 5.2 Run `openspec validate layout-writing-rules --strict`
