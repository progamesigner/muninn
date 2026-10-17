# Verification

## Coverage

| Dimension | Evidence |
| --- | --- |
| Completeness | 7/7 tasks complete; all four capability requirements implemented, checked, and synchronized before archival. |
| Correctness | Versioned snapshot, precondition, byte/line cap, scope/grant/policy/visibility, wikilink/index, managed-mutation, optional-context, and independent-process races covered. |
| Coherence | Uses existing scoped wrappers, structured errors, atomic write procedure, schema normalization, link transforms, and index notifications. Default read and foundational context behavior remain compatible. |

No missing implementation or uncovered required scenario was found. Content versions intentionally identify identical stored bytes, including A-to-B-to-A changes; supported concurrent writers cooperate through the wrapper and a filesystem with advisory locks and atomic rename.

## Requirement evidence

- Versioned note reads: `src/tools.rs` single-read handler hashes the one raw snapshot before stripping links/slicing. `brief_versions_round_trip_and_conflicts_are_structured_errors` verifies optional metadata, range/backlinks composition, and unchanged default shape.
- Scoped conditional wrapper: `update_agents_file` reuses scope/grant, bounded wrapper preparation, and gated writes/index notifications. Tests cover required preconditions, UTF-8 and expanded-byte limits, configured agents root/empty scheme, scope isolation, ignored files, read-only policy, and wikilink/index round trips.
- Atomic conflict detection: `Storage::write_conditional` holds both a process mutex and a persistent sidecar OS lock across comparison and atomic replacement. `tests/conditional_storage.rs` holds the OS lock in the parent, then races independent child processes for both existing and absent targets. Both races admit exactly one writer; failures preserve the winner.
- Managed brief boundaries: generic mutations, batch prevalidation, indirect rename rewrites, and directory aliases are refused in `src/tools.rs`. The tools tests verify unchanged source/destination/brief and the unchanged five-file session context. Schema snapshots and transport tool counts include the new wrapper.
- Huginn integration: the matching `feat/m7` branch negotiates `include_version`, uses structured result retrieval without changing ordinary text calls, and preserves error discriminators. Its local integration fixture clears the server environment, launches loopback-only real binaries against a temporary vault, and cleans up every process. A scripted model exercises the actual Huginn runner without paid provider calls.

## Checks

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo test --all-features`: 494 passed, no ignored tests.
- `cargo test`: 461 passed, no ignored tests.
- `openspec validate add-versioned-collaboration-brief --strict`: passed before archival.
- `openspec validate collaboration-brief --type spec --strict`: passed.
- `openspec validate --specs`: 11 passed; delta and main requirement/scenario bodies compared equal before archival.
- Huginn: ruff check/format, ty, import-linter (three contracts), example config validation using CI dummy credentials, and `uv lock --check`: passed.
- Huginn full suite without local binary opt-in: 1730 passed, 13 skipped, three existing warnings. The skips include ten external credential-dependent live tests and the three local Muninn tests.
- Final relevant regression with local binary: 43 passed (40 unit tests plus three real local-server tests). Final full suite with `MUNINN_TEST_BINARY` enabled: 1733 passed, 10 skipped, three existing warnings. All three real local-server tests are included; only the ten external credential-dependent live tests remain skipped.
- Both feature branches pushed and remote heads matched before archival. Every new commit has a valid GPG signature, a single English subject, and no body/trailers.

## Existing documentation warnings and rollout limits

An extra `openspec validate --specs --strict` check fails on six unchanged legacy Purpose placeholders: configuration, container-image, context-http-api, mcp-server, memory-tools, and vault-storage. Standard whole-spec validation and strict validation of this change/new capability pass. These unrelated existing specs were preserved.

No main merge, PR, release, tag, deployment, production memory E2E, or paid model call was performed. Future rollout must validate locks/atomic rename on the deployed shared filesystem across hosts and ensure every concurrent service uses the conditional wrapper. Noncooperating external editors are outside advisory-lock guarantees. Provider cached-token/cost measurements and real Telegram/production acceptance remain rollout work.

Archive: `openspec/changes/archive/2026-10-17-add-versioned-collaboration-brief/` (UTC date; Taiwan progress notes are dated 2026-10-18). All seven tasks complete; no active change remains.
