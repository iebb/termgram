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

## Caching

Termgram has three cache layers. Extend the existing one that fits instead of adding a new mechanism.

- Persistent store (`src/cache.rs`, `Store`): account-local, discardable libsql database for anything that should survive restarts — history, reply previews, pins, sticker sections, downloaded attachments, and media preview thumbnails. New persistent data means a schema migration (`SCHEMA_VERSION` + `user_version`), writes through `Store::apply(&[NetworkEvent])`, invalidation when the underlying message/media changes, orphan cleanup alongside the existing prune queries, and stale-while-revalidate serving in `src/telegram/local.rs::serve_cached`.
- Media file cache (`src/telegram/media_cache.rs`): bounded on-disk storage (512 files / 1 GiB, oldest mtime first) under `<session>.media/<account>/`. Every downloaded file lands here via `media_cache::temporary` + `finish`, and its path is registered in the `Store` so cache hits skip the network; never key downloads by ad-hoc paths outside this directory.
- Bounded in-memory caches (`WorkerCache` in `src/telegram/mod.rs`, app-side maps like `chat_info.rs`): for session-scoped or quasi-static network values. They must have an explicit size bound and, for server-owned values, a TTL or invalidation trigger.

Rules of thumb:

- Every cache must be bounded (entries, bytes, or TTL). Unbounded growth is a bug.
- Cache reads must tolerate missing or stale entries: treat vanished files and mismatched media ids as misses, never as errors.
- Quasi-static RPC results are good TTL-cache candidates; message/media-derived data belongs in the `Store` with event-driven invalidation, not TTLs.

Known worthwhile candidates (surveyed 2026-09, not yet implemented):

1. Memoize transcript rendering: `render_conversation` re-renders every message of the active chat on every frame (`src/ui/mod.rs:621`, `src/ui/transcript.rs`); cache rendered lines keyed by (message id, width, selection, content revision).
2. Partial eviction in the encoded-image cache: any visible-set change clears all encoded protocols (`src/media.rs:111`); retain entries whose protocol allows it (Halfblocks/Sixel/iTerm2), keep the Kitty re-encode behavior.
3. Session-cache `help::GetConfig` results: re-fetched on every delete review and edit (`src/telegram/deletion.rs`, `src/telegram/editing.rs`); revoke/edit limits are quasi-static.
4. Share one full-chat-info cache between the `:info` popup (`src/app/chat_info.rs`, 60s TTL) and the reactions panel, which re-runs `GetFullChat`/`get_me` on every open (`src/telegram/reactions.rs`).
5. TTL-cache account-wide notify defaults: `GetNotifySettings` runs twice per dialog refresh (`src/telegram/folders.rs:79`).
6. Memoize `filtered_chat_indices` (`src/app.rs:2151`), recomputed on every chats-pane frame; key by (filter text, folder id, chats revision).

## Git workflow and commit messages

- Implement each task on a descriptive branch, such as `feat/yazi-terminal-backend`.
- Make atomic commits: one coherent, reviewable purpose per commit, with required documentation and dependency lockfile changes included. Each implementation commit must build; avoid intermediate broken commits or unrelated changes.
- Use English Conventional Commit subjects: `<type>(<scope>)!: <imperative summary>`; `!` is required only for breaking changes. Use `feat`, `fix`, `refactor`, `build`, `docs`, `test`, or `chore`; choose a concrete scope such as `terminal`, `deps`, or `agents`.
- Keep subjects concise (prefer at most 72 characters), imperative, and without a trailing period. Avoid vague messages such as "update", "misc fixes", and "WIP".
- For nontrivial changes, explain the problem, resulting behavior, and relevant validation in the body. Include `BREAKING CHANGE:` with the impact and migration for breaking changes. Record upstream revisions where relevant.
- Commit only task-related files. Do not push, merge, or publish unless the user requests it.
