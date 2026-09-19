# Termgram engineering principles

## Reuse before implementation

- Prefer excellent maintained libraries and proven upstream code over handwritten infrastructure. Inspect Yazi and Codex implementations before building terminal, input, rendering, configuration, or plugin machinery.
- Direct crate dependencies are welcome. Pin Git dependencies to immutable revisions and commit the lockfile. Do not depend on sibling checkouts or machine-local paths.
- Replacing existing Termgram code with a better upstream implementation is encouraged. Preserve upstream compatibility fixes and complete lifecycles; keep application adapters small.
- Rust toolchain and edition upgrades, additional dependencies, and breaking changes are authorized when they improve the implementation. Remove superseded paths instead of carrying compatibility layers without a concrete need.
- Unsafe code is allowed where justified, especially in proven platform libraries. Keep unsafe boundaries explicit, document safety invariants for locally written unsafe code, and prefer an existing safe interface when available.
- For copied code, record the upstream repository, revision, original paths, and local adaptations. Preserve applicable LICENSE and NOTICE files. Prefer depending on the original crate when that avoids maintaining a fork.

## Design and validation

- Do not use TDD. Understand the upstream implementation and its callers, decide the design, implement it, and only then validate it.
- Keep tests minimal and focused on meaningful behavior or integration boundaries. Reuse relevant upstream tests; do not duplicate dependency test suites, mirror implementation details, or build a general testing framework.
- Temporary scaffolding, diagnostic fixtures, smoke-test programs, and exploratory tests must be removed once the implementation is stable. Keep only small, durable regression tests with a clear purpose.
- Run the relevant existing checks. Do not repeatedly run or expand tests after they pass without a new change, failure, or unresolved concern.
- Terminal input has one reader. Terminal modes and restoration have one owner. Coordinate text and graphics output; preserve cleanup on initialization failure, normal exit, and panic.
- Preserve useful business behavior while replacing infrastructure. Do not retain obsolete abstractions merely to minimize the diff.

## Git workflow and commit messages

- Implement each task on a descriptive branch, such as `feat/yazi-terminal-backend`.
- Make atomic commits: one coherent, reviewable purpose per commit, with required documentation and dependency lockfile changes included. Each implementation commit must build; avoid intermediate broken commits or unrelated changes.
- Use English Conventional Commit subjects: `<type>(<scope>)!: <imperative summary>`; `!` is required only for breaking changes. Use `feat`, `fix`, `refactor`, `build`, `docs`, `test`, or `chore`; choose a concrete scope such as `terminal`, `deps`, or `agents`.
- Keep subjects concise (prefer at most 72 characters), imperative, and without a trailing period. Avoid vague messages such as "update", "misc fixes", and "WIP".
- For nontrivial changes, explain the problem, resulting behavior, and relevant validation in the body. Include `BREAKING CHANGE:` with the impact and migration for breaking changes. Record upstream revisions where relevant.
- Commit only task-related files. Do not push, merge, or publish unless the user requests it.
