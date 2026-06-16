# Contributing to CEESVEE

Thanks for your interest in improving CEESVEE! This guide covers how to get set
up and the standards we hold contributions to.

## Getting started

1. Fork and clone the repository.
2. Install dependencies and run the app:

   ```bash
   npm install
   npm run tauri dev
   ```

   You'll need Node.js 18+, a stable Rust toolchain, and the
   [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your
   platform.

## Project layout

```
.
├── src/                # React + TypeScript front end
│   ├── components/     # UI (grid, toolbar, dialogs, …)
│   ├── lib/            # pure helpers + the typed Tauri command layer
│   ├── store/          # Zustand store
│   └── types.ts        # TS mirrors of the Rust DTOs
└── src-tauri/          # Rust core
    └── src/
        ├── parse.rs    # parsing + encoding/delimiter detection
        ├── document.rs # in-memory model + undo/redo
        ├── export.rs   # serialization
        ├── find.rs     # find / replace
        ├── sort.rs     # multi-key sort
        └── commands.rs # the Tauri command surface
```

The guiding principle: **Rust owns the data; the front end owns rendering.** New
data operations belong in the Rust core and are exposed as commands; the UI
fetches only the row windows it needs.

## Before you open a PR

Run the full local check suite — these mirror CI and must pass:

```bash
# Front end
npm run lint
npm run typecheck
npm test
npm run build

# Rust
cargo fmt   --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path src-tauri/Cargo.toml
```

`npm run format` (Prettier) and `cargo fmt` will fix most formatting for you.

## Coding standards

- **TypeScript** is in strict mode. No `any` escape hatches without a good reason.
- **Rust** must be `clippy`-clean (`-D warnings`) and `cargo fmt`-formatted.
- Add tests for new logic. Rust parsing/model changes should include unit tests,
  ideally a **round-trip** assertion (parse → serialize → parse yields identical
  data). Core front-end logic gets a Vitest test.
- Never crash on bad input — surface a clear error message instead.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(core): add semicolon delimiter detection
fix(grid): keep dirty highlight aligned after sort
docs: expand the build-from-source section
chore(ci): cache the cargo registry
```

Common types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `chore`, `ci`.

## Releasing

Releases follow [semantic versioning](https://semver.org/). Bump the version in
`package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`, then
push a `vX.Y.Z` tag — the release workflow builds and attaches installers for
Windows, macOS, and Linux.

## Reporting bugs & requesting features

Use the issue templates. For bugs, include your OS, a sample file (or its shape),
and the steps to reproduce. Thank you! 💜
