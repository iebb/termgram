# Upstream code

Yazi is pinned to `9203fd2604f867ab5ec18f24203b918975c4c00a` from
https://github.com/sxyazi/yazi. All Yazi crates except `yazi-term` are Git
dependencies at that revision.

`yazi-term/` copies the complete upstream `yazi-term/src` and its MIT license.
Its standalone manifest resolves upstream workspace dependencies explicitly.
The only source changes expose mouse coordinates/modifiers, key state, and
`KeyEvent::new` to Rust consumers; upstream exposes some of these only to Lua.
Parsing, platform input, timeouts, and restoration code are unchanged.

When updating, replace from one upstream revision, reapply these visibility
changes, update all Yazi Git revisions together, and review the integration in
`src/terminal.rs`. Keep vendored formatting and do not run Termgram's lint or
formatting policy over upstream source.

The integration in `src/terminal.rs` adapts the mode setup/cleanup sequences from
`yazi-tui/src/raterm.rs` and the two-stage probe handling from
`yazi-actor/src/app/report.rs` and `app/passthrough.rs` at the same revision.
These are covered by `licenses/Yazi-MIT.txt`. Termgram makes tmux option changes
explicitly opt-in and removes the actor/proxy dispatch layer. The upstream
`yazi-emulator`, `yazi-tty`, parser, reader, and platform restorer remain in use.

`yazi-term/LICENSE` retains the separate Michael Davis and sxyazi attribution.

`yazi-image/icc.rs` copies `yazi-adapter/src/icc.rs` unchanged from the same
Yazi revision, under `licenses/Yazi-MIT.txt`. This private upstream helper retains
ICC-to-sRGB conversion while `ratatui-image` owns multi-image encoding and clipped
rendering. `src/media.rs` also follows Yazi's EXIF orientation handling and Kitty
cleanup sequence. Protocol selection still uses the original `Drivers::matches`;
old Kitty-only implementations and external-overlay drivers fall back to the
library's Unicode half-blocks. The former single-surface adapter initialization
and embedded Yazi presets are no longer needed.
