# Repository Guidelines

## Project Structure & Module Organization

Incline Design is a Rust 2024 desktop/WebAssembly mine design application using `winit`, `wgpu`, and `egui`.

- `src/app/`: application state, command handling, and background jobs.
- `src/model/`: domain geometry, project persistence, and import/export formats.
- `src/rendering/`: GPU rendering, scene caches, picking, and WGSL shaders.
- `src/ui/`: widgets, dialogs, menus, and editor state; `src/mac.rs` implements native macOS menus.
- `res/`: embedded assets; `i18n/`: Fluent translations; `docs/`: documentation assets.
- `examples/`: import-ready data for manual validation. Format fixtures live under `src/model/formats/fixtures/`.

## Task Entry Points & Efficient Navigation

Paths below are relative to `src/`:

| Task | Start here / rule |
| --- | --- |
| Add a UI action | `ui/state.rs` (`UiCommand`, `console_report_spec`), UI call site, `app/commands/mod.rs` (dispatch, `requires_project`). |
| Change state | `app/mod.rs` owns durable state; `ui/state.rs` owns transient `EditorState`; `model/project.rs` manages projects. |
| Fix rendering | `rendering/graphics/init.rs` (pipelines), `passes.rs` (draw passes), `rendering/scene/` (geometry/cache), `rendering/shaders/` (WGSL). |
| Add background work | Reuse `app/jobs.rs`: compute on workers, apply on the UI thread; preserve `JobKey` dependencies and poll cancellation in long loops. |
| Change persistence | `model/formats/`, `model/atomic_file.rs` (native writes), `app/web_storage.rs` (browser storage). |

Search the relevant subtree first, e.g. `rg -n 'draw_screen_cross' src/rendering`. Read matching functions and nearby callers before whole files. Exclude generated `target/` and `dist/` from code searches. Run checks appropriate to the change; repeat only after edits or unresolved failures. Keep these pointers current when moving code.

## Build, Test, and Development Commands

Use `rust-toolchain.toml`'s nightly toolchain. Linux requires `clang` and `mold`; `.cargo/config.toml` enables standard-library rebuilding.

- `cargo check`: check compilation.
- `cargo run`: launch the desktop application.
- `cargo build --release`: build an optimized desktop binary.
- `cargo fmt --check` / `cargo fmt`: check/apply formatting.
- `cargo clippy`: run Rust lints.
- `cargo test`: run the Rust test harness.
- `trunk serve`: serve the web application at `127.0.0.1:8080`.
- `trunk build --release`: build web assets into `dist/`.

## Coding Style & Naming Conventions

Follow `rustfmt.toml`: four spaces, Unix newlines, 180 columns, and standard/external/crate import groups. Use `snake_case` for functions/modules, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants.

Route UI changes through `UiCommand` and application handlers. Preserve revision-based cache invalidation; avoid unconditional scene rebuilds. Keep domain coordinates in double precision and rebase positions before GPU conversion. Gate platform-specific code appropriately for WebAssembly.

## Architecture Rules That Prevent Rework

- Use `SceneEntityId` for selection and viewport queries across entity types; start in `src/rendering/pick.rs` and `query.rs`.
- For larger editor transitions, use `EditorState::apply_action(EditorAction)` and propagate its geometry-dirty result.
- `ProjectItemState::revision` invalidates caches; `epoch`/`saved_epoch` determine unsaved changes. Undo restores the content epoch while advancing revision; preserve this distinction (`src/model/project.rs`).
- File/thread changes need native and WebAssembly paths. Browser persistence uses IndexedDB; wasm panics abort, so worker panic recovery cannot help. Preserve Trunk's COOP/COEP headers for shared memory.

## UI, Translation & Logging Conventions

- Use `tr!("message-id")` for user-facing text; add keys to `i18n/en/Incline_Design.ftl`. Pass named values with `tr!("greeting", name = who)`. Existing literal-style code supports `tr!(literal = "Apply")` and `tr_format!` for placeholders; see `src/i18n.rs`.
- Use `userspace_log!`, `userspace_warn!`, and `userspace_error!` for activity-console messages, e.g. `userspace_log!("{}", tr!(literal = "Completed"))`. Reserve `log::` macros for diagnostic logging.
- Use `themed_icon!(ui, "name.svg")` or `unthemed_icon!("name.svg")` for embedded icons, and `widgets::toolbar::GROUP_CORNER_RADIUS` for rounded UI elements.
- Keep egui menus, context menus, and native macOS menus synchronized when changing actions.
- New top-level panels must use `chrome::region_frame` and pass their rectangle to `chrome::paint_regions` in `src/ui/mod.rs`; nested panels do neither. See `src/ui/chrome.rs`.

## Testing Guidelines

No dedicated test suite or coverage threshold exists. For substantive logic changes, add focused `#[test]` cases in adjacent `#[cfg(test)]` modules with behavior-based names; run with `cargo test <name>`. Once the tests passes and you are satisified with the diff, please remove the test; it is no longer needed and should not be commmited.

Validate rendering/interactions using `examples/`, including relevant camera angles and desktop/web behavior. Report checks and validation gaps.

## Commit & Pull Request Guidelines

Use short, action-oriented commit subjects; history has no mandatory prefix scheme.

Keep PRs focused; explain the problem, resulting behavior, and validation. Link relevant issues, include screenshots for visual changes, and identify platform limitations.
