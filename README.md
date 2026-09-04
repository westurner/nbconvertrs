# nbconvertrs

`nbconvertrs` is the Rust transform library and command-line tool used by
sustainablefactory and DocIndex to convert Markdown, MyST, scripts, and
Jupyter notebooks. It uses the `runtimed` `nbformat` model and keeps the
library independent from filesystem indexing details.

The canonical binary is `nbconvertrs`. The `transform-md` binary is retained
as a compatibility name for existing sustainablefactory workflows.

## Supported formats

### Markdown and MyST

- Ordinary Markdown becomes Markdown cells.
- Fenced code blocks become code cells. Language identifiers are preserved.
- MyST `{code-cell}` and `{raw-cell}` fences are supported.
- Cell YAML front matter and colon-prefixed cell options are preserved.
- HTML regions support `#region`, `#markdown`, and `#raw` markers with their
  matching end markers.
- Notebook-level YAML front matter is preserved.
- `m1` cell splitting starts a new Markdown cell at level-one headings.

### Script formats

- Percent scripts use `# %%` for Python, R, and Julia-style comments, or `% %%`
  for MATLAB.
- Light scripts use `# +` and `# -`, or the corresponding language prefix.
- Markdown and raw script cells use `[markdown]` and `[raw]` markers.
- Python, R, Julia, and MATLAB input extensions are recognized by the file
  transform API.

### Notebook JSON

Existing v4 notebooks can be read and written without dropping notebook
metadata, cell metadata, attachments, execution counts, outputs, or stable
cell IDs. Legacy and v3 notebooks are upgraded through `nbformat` when
possible. JSON output is deterministic for equivalent notebook structures.

## Library API

```rust
use nbconvertrs::{markdown_to_notebook, notebook_to_json, TransformOptions};

let notebook = markdown_to_notebook(
    "# Title\n\n```python\nprint(1)\n```\n",
    &TransformOptions::default(),
)?;
let json = notebook_to_json(&notebook)?;
```

The main entry points are:

- `markdown_to_notebook(source, options)`
- `script_to_notebook(source, format, language)`
- `notebook_to_markdown(notebook)`
- `notebook_to_script(notebook, format, language)`
- `notebook_to_json(notebook)`
- `transform_file(source, output_base, formats, options)`
- `transform_manifest(manifest, dry_run)`
- `load_workflow_config(path)`

`transform_file` accepts output formats such as `myst`, `ipynb`,
`py:percent`, and `py:light`. It returns the paths and format names of the
files it wrote.

## CLI

Build or run the canonical binary from the workspace:

```text
cargo run --manifest-path Cargo.toml --bin nbconvertrs -- --help
```

Single-file conversion:

```text
nbconvertrs document.md --output build/document --out-format=myst,ipynb
```

Directory conversion:

```text
nbconvertrs --indir docs --outdir build/docs --out-format=myst,ipynb
```

Incremental workflow conversion uses a JSON manifest. A `_toc.yml` can supply
the manifest path, output formats, and transform settings:

```text
nbconvertrs --config docs/_toc.yml
nbconvertrs --manifest .tmp/workflow/chat-manifest.json --dry-run
```

Workflow mode compares source SHA-256 values, transform fingerprints, and
output existence before transforming. Successful outputs are written in a
temporary directory and moved into place together; the manifest is updated
only after the outputs succeed.

## Tests and coverage

Run the focused test suite:

```text
cargo test --manifest-path Cargo.toml
```

Generate focused library coverage when `cargo-llvm-cov` is installed:

```text
cargo llvm-cov --manifest-path Cargo.toml --lib --summary-only
```

The tests cover Markdown/MyST markers, HTML regions, percent and light
scripts, upstream Jupytext fixtures, notebook preservation, round trips, and
incremental workflow behavior.

## Compatibility notes

This crate implements the supported transform subset in Rust. It does not
execute notebooks or kernels. It also does not attempt to reproduce every
Jupytext format, language-specific option, or execution feature. Unsupported
output formats return `TransformError::UnsupportedFormat` so callers can
choose a Python/Jupytext fallback when their workflow requires a broader
format matrix.

The transform crate is nested under sustainablefactory in the dsport
workspace. Changes here must be committed in this repository before the
sustainablefactory parent can update its submodule pointer.
