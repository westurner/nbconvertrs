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
- The language registry recognizes Python, R, Julia, MATLAB, JavaScript,
  TypeScript, Ruby, shell, Rust, and SQL prefixes and extensions.
- Input formats are detected from notebook extensions, registered script
  extensions, and percent markers, with explicit `--from` overrides.

### Notebook JSON

Existing v4 notebooks can be read and written without dropping notebook
metadata, cell metadata, attachments, execution counts, outputs, or stable
cell IDs. Legacy and v3 notebooks are upgraded through `nbformat` when
possible. JSON output is deterministic for equivalent notebook structures.

### HTML

- `html` produces a deterministic static HTML document for Markdown, raw, and
  code cell source.
- Cell source is HTML-escaped; notebook execution and rich MIME output
  rendering are intentionally deferred to the resource/exporter milestones in
  `docs/PARITY_PLAN.md`.

### Static text exporters

- `rst` and `rest` render code cells as reStructuredText `code-block`
  directives.
- `asciidoc` and `adoc` render code cells as AsciiDoc source blocks.
- `quarto`/`qmd` and `pandoc` provide Markdown-oriented one-way output names;
  their format-specific execution directives are not interpreted.
- These formats are output-only and cannot be used as synchronization sources.

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

- `FormatId`, `ScriptKind`, `FormatDescriptor`, and `language_specs()` for
  typed format discovery.
- `Exporter`, `BasicExporter`, `ExportResult`, `ResourceBundle`, and
  `export_notebook(notebook, format)` for in-memory conversion.
- `NotebookDocument`, `TextDocument`, `Converter`, and `BasicConverter` for
  filesystem-free conversion composition.
- `Preprocessor`, `PreprocessorPipeline`, `ClearOutputs`,
  `ResetExecutionCounts`, `RemoveTaggedCells`, and `RegexRemove` for ordered
  notebook preprocessing.
- `FileWriter`, `Writer`, and `write_export` for atomic file output and
  extracted resources.
- `markdown_to_notebook(source, options)`
- `script_to_notebook(source, format, language)`
- `notebook_to_markdown(notebook)`
- `notebook_to_script(notebook, format, language)`
- `notebook_to_json(notebook)`
- `transform_file(source, output_base, formats, options)`
- `transform_file_with_input_format(source, output_base, formats, options,
  input_format)`
- `source_to_notebook(source, format, options)` and
  `export_notebook_with_options(notebook, format, options)`
- `sync_pair(notebook, text, format, options)` for conflict-aware pair
  synchronization with atomic writes.
- `transform_manifest(manifest, dry_run)`
- `load_workflow_config(path)`

`transform_file` accepts output formats such as `myst`, `ipynb`, `html`,
`py:percent`, and `py:light`. It returns the paths and format names of the
files it wrote. Format aliases are normalized internally while the requested
format name is retained in each `TransformOutput`.

## CLI

The CLI accepts one input selector and one or more output formats. Format names
are normalized through the same registry used by the Rust API, so aliases and
script languages do not need separate CLI rules.

### Input and output formats

The following single table is the format vocabulary for `--from`, `--to`, and
`--out-format`. `Input` and `Output` describe the direction supported by the
current native implementation.

| Family | Names | Extensions | Input | Output |
| --- | --- | --- | --- | --- |
| MyST Markdown | `myst`, `md`, `markdown` | `.md`, `.markdown`, `.myst.md` | yes | yes |
| Notebook JSON | `ipynb`, `notebook` | `.ipynb` | yes | yes |
| Quarto Markdown | `quarto`, `qmd` | `.qmd` | no | yes |
| Pandoc Markdown | `pandoc` | `.md` | no | yes |
| HTML | `html` | `.html` | no | yes |
| reStructuredText | `rst`, `rest` | `.rst`, `.rest` | no | yes |
| AsciiDoc | `asciidoc`, `adoc` | `.adoc`, `.asciidoc` | no | yes |
| Percent script | `<language>:percent` | language-specific | yes | yes |
| Light script | `<language>:light` | language-specific | yes | yes |

Script languages are registered once and use the same `<language>:<kind>` form
in both columns: `python` (`py`), `r` (`R`, `r`), `julia` (`jl`), `matlab`
(`m`), `javascript` (`js`), `typescript` (`ts`), `ruby` (`rb`), `shell`
(`sh`, `bash`), `rust` (`rs`), and `sql` (`sql`). For example,
`py:percent` selects a Python percent script and `javascript:light` selects a
JavaScript light script.

Input detection uses explicit `--from` first, then notebook/Quarto/static-text
extensions, registered script extensions and markers, and finally Markdown as
the fallback. HTML, RST, and AsciiDoc are deliberately output-only.

### Commands

Build or run the canonical binary from the workspace:

```text
cargo run --manifest-path Cargo.toml --bin nbconvertrs -- --help
```

Single-file conversion:

```text
nbconvertrs document.md --output build/document --out-format myst,ipynb
nbconvertrs document.md --output build/document --to py:percent
nbconvertrs source.py --from py:percent --output build/document --to ipynb
nbconvertrs document.md --to rst --stdout
cat document.md | nbconvertrs - --from myst --to html --stdout
```

Directory conversion:

```text
nbconvertrs --indir docs --outdir build/docs --out-format=myst,ipynb
```

`--indir` recursively selects `.md` and `.markdown` files. Each output keeps
the source-relative path beneath the destination directory.

Incremental workflow conversion uses a JSON manifest. A `_toc.yml` can supply
the manifest path, output formats, and transform settings:

```text
nbconvertrs --config docs/_toc.yml
nbconvertrs --manifest .tmp/workflow/chat-manifest.json --dry-run
```

Synchronize one notebook with one text representation. The notebook is the
positional source and `--output` names the paired text file:

```text
nbconvertrs note.ipynb --sync --output note.md --to myst
```

When both files differ, the newer file is used. Equal-timestamp divergence is
reported as a conflict rather than overwritten automatically.

Workflow mode compares source SHA-256 values, transform fingerprints, and
output existence before transforming. Successful outputs are written in a
temporary directory and moved into place together; the manifest is updated
only after the outputs succeed.

The CLI also preserves the existing `transform-md` binary name. Run
`nbconvertrs --help` for the short command summary and examples; this README's
format table is the canonical input/output list.

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

## Fuzzing

The `fuzz/` package contains deterministic libFuzzer targets for the public
conversion APIs. It is a separate Cargo workspace and is not included in the
production crate workspace. Install `cargo-fuzz` and a nightly Rust toolchain,
then run from the fuzz package directory:

```text
cd fuzz
cargo +nightly fuzz build
cargo +nightly fuzz run markdown_to_notebook -- -max_total_time=10
cargo +nightly fuzz run script_to_notebook -- -max_total_time=10
cargo +nightly fuzz run notebook_json -- -max_total_time=10
cargo +nightly fuzz run export_dispatch -- -max_total_time=10
```

The checked-in corpus covers valid and malformed Markdown, percent and light
scripts, notebook JSON, metadata, mixed newlines, Unicode, and export format
dispatch. Expected parse errors are normal fuzz results. A successful parse
must serialize to valid notebook JSON and remain structurally stable after a
second parse; a panic or invariant violation produces a minimized artifact in
`fuzz/artifacts/`. Generated targets and coverage data are ignored by Git.

## Compatibility notes

This crate implements the supported transform subset in Rust. It does not
execute notebooks or kernels, render LaTeX/PDF/slides, or provide Jinja
templates and the complete nbconvert preprocessor catalog. Its static HTML exporter does not
yet render rich MIME outputs, although supported binary MIME data is available
through `ResourceBundle`. It also does not attempt to reproduce every Jupytext
format or language-specific option. Unsupported output formats return
`TransformError::UnsupportedFormat` so callers can choose a Python/Jupytext or
nbconvert fallback when their workflow requires a broader format matrix. The
staged implementation roadmap is in
[`docs/PARITY_PLAN.md`](docs/PARITY_PLAN.md).

## Citation

This project follows the text-notebook formats and conventions established by
`ipython nbconvert` and Jupytext:

> "Jupytext: Jupyter notebooks as Markdown documents, Julia, Python or R
> scripts." [Jupytext project](https://github.com/jupytext/jupytext).


## License

`nbconvertrs` is distributed under the [BSD-3-Clause license](LICENSE). The
license text follows the [runtimed BSD-3-Clause license](https://github.com/runtimed/runtimed#BSD-3-Clause-1-ov-file).