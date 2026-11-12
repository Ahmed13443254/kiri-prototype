# kiri

An experimental desktop app for managing AI coding-agent sessions, built with React, TypeScript, Rust, and Tauri.

> ## Project status: early prototype
>
> **kiri is not ready for production use.** It is an early prototype intended for initial testing, exploration, and feedback. Features may be incomplete, unstable, or subject to significant change.

Arabic is the primary interface language today. The interface and content architecture are being developed with multilingual support in mind, and additional languages will be added over time.

## Current features

- Responsive Arabic RTL interface for managing projects and conversations.
- Persisted sessions with search, resume, and forking.
- Model, effort level, and permission-mode controls.
- Rendering for responses, tool calls, reasoning, and file diffs.
- Screens for rules, tasks, extensions, context, memory, and settings.
- OS credential-vault storage for the model API key in the desktop app.

## Repository structure

| Directory | Purpose |
| --- | --- |
| `src/` | React and TypeScript interface |
| `engine/` | Rust agent engine communicating over NDJSON and stdin/stdout |
| `src-tauri/` | Tauri host, SQLite database, and desktop commands |
| `tests/` | Engine and storage probes |

## Requirements

- A recent Node.js release and npm.
- Rust and Cargo.
- A C compiler for the bundled SQLite build used by the Tauri host.
- The platform requirements for Tauri.

## Run the interface

```bash
npm install
npm run dev
```

The interface runs locally at `http://localhost:1420`. Running it in a browser alone displays the UI, but engine-backed features require the Tauri host.

## Build the engine and desktop app

```bash
cargo build --manifest-path engine/Cargo.toml --release
npm run build
npm run tauri build
```

On Windows, avoid spaces in the project path because some MinGW tools can fail while building Tauri from such a path. The full `tauri dev` workflow has not been verified on the current development machine because of a MinGW linker issue.

## API key

Never commit an API key to the repository or a public configuration file.

- The desktop app stores the key in the OS credential vault.
- When running the engine directly, provide `ANTHROPIC_API_KEY` through the environment.
- A compatible endpoint can be configured with `KIRI_BASE_URL`.

## Verification

```bash
npm run build
cargo test --manifest-path engine/Cargo.toml
```

The interface build, screen navigation, model selection, and composer enablement have been checked. The engine test suite currently passes 32 tests. Not every engine workflow has been retested since the interface redesign, so kiri remains an early prototype rather than a production-ready release.

## Development notes

- Local files, secrets, databases, build output, and test work directories are excluded through `.gitignore`.
- If an API key was ever exposed in terminal output or elsewhere, revoke it with the provider and issue a new one. Removing it from files does not invalidate the credential.
- The desktop host currently needs additional verification across platforms before it can be considered stable.

## License

Licensed under the Apache License 2.0. See [LICENSE](LICENSE).
