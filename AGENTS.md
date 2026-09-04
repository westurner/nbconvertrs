# AGENTS.md

## Purpose

`nbconvertrs` is the Rust implementation of the sustainablefactory Markdown,
script, and notebook transform workflow. Keep its behavior compatible with
Jupytext where the supported format subset is documented in `README.md`.

## Repository boundary

This directory is a nested Git repository. Commit transform-library and CLI
changes here first. The `src/sustainablefactory` parent repository tracks the
resulting submodule pointer separately. Do not modify vendored upstream
sources in neighboring repositories unless the task explicitly requires it.

## Source map

- `src/lib.rs`: conversion APIs, notebook metadata handling, workflow
  manifests, and filesystem transforms.
- `src/main.rs`: `nbconvertrs` and compatibility `transform-md` CLIs.
- `src/transform_md.rs`: compatibility binary entry point.
- `tests/`: integration fixtures and behavior tests.
- `tools/`: generators and maintenance helpers.

## Development rules

- Preserve public APIs and existing CLI spellings unless compatibility needs a
  deliberate change.
- Prefer a focused test before or alongside each behavior change.
- Match upstream Jupytext behavior when practical; document accepted
  deviations in `README.md` or test names.
- Use structured notebook and YAML APIs. Do not manipulate notebook JSON with
  ad hoc string replacement.
- Keep notebook JSON valid. Every cell must contain `metadata.language`.
  Existing cells must retain `metadata.id`; newly generated cells may omit an
  ID when the upstream model does so.
- User-facing test descriptions and diagnostics refer to cell numbers, never
  cell IDs.
- Preserve attachments, outputs, execution counts, notebook metadata, and
  cell metadata during round trips unless a format intrinsically cannot carry
  the value.
- Use atomic output behavior for workflow transforms and do not update a
  manifest when an output transform fails.
- Keep changes ASCII by default and avoid unrelated formatting churn.

## Validation

From this directory, run:

```text
cargo test --manifest-path Cargo.toml
cargo llvm-cov --manifest-path Cargo.toml --lib --summary-only
cargo fmt --all -- --check
```

For a narrow change, run the smallest relevant test first. Before a commit,
run the complete focused test suite and confirm coverage output. Coverage work
should add tests for the behavior represented by uncovered lines rather than
exclude production code from instrumentation.

## Commit guidance

Use one focused commit per logical change. Commit messages should explain:

1. What behavior changed.
2. Why the change is needed for Jupytext or workflow compatibility.
3. Which tests and coverage checks were run.

Do not commit generated build directories, temporary manifests, or unrelated
parent-repository changes. After committing, verify the message with:

```text
git log -1 --format=%B
```
