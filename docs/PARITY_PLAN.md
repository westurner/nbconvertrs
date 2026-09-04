# nbconvertrs parity plan

## Purpose

This document defines a staged plan for bringing `nbconvertrs` toward feature
parity with the two projects that set the compatibility target:

- [Jupytext](https://github.com/jupytext/jupytext), for loss-aware conversion
  between notebooks and text representations.
- [nbconvert](https://nbconvert.readthedocs.io/), for notebook export,
  preprocessing, execution, templating, and resource management.

The target is a useful Rust-native implementation with compatible behavior at
the file, library, and command-line boundaries. It is not a promise to copy
internal Python architecture or to reimplement a Python kernel in Rust.

Parity is measured at three levels:

1. **Wire parity**: the same input produces an equivalent notebook, text file,
   rendered document, or resource set.
2. **Behavioral parity**: the same options, metadata, errors, and output naming
   rules have the same observable meaning.
3. **Extension parity**: users can register formats, preprocessors, templates,
   writers, and execution backends through documented Rust interfaces.

Every feature in this plan must identify its parity level and record accepted
differences. Unsupported behavior should produce a specific diagnostic and a
clear fallback path rather than silently losing notebook content.

## Implementation status

The current implementation has completed the conversion core, common script
format registry, pair synchronization, and the first export-foundation slice.
The foundation now includes typed notebook/text documents, stable error
categories, deterministic resource extraction for common binary MIME values,
an atomic file writer, an ordered preprocessor pipeline, and native one-way
RST and AsciiDoc exporters. Quarto and Pandoc-oriented Markdown output names
are also registered as explicit one-way formats. These additions provide
semantic parity for the documented subset; they do not claim full Jupytext or
nbconvert parity.

## Current baseline

`nbconvertrs` currently provides:

- A Rust library and CLI for Markdown, MyST, script, and notebook transforms.
- Markdown code and raw fences, notebook YAML front matter, cell options, and
  selected HTML-region markers.
- Jupytext-style percent and light scripts for Python, R, Julia, and MATLAB
  prefixes.
- Notebook JSON read/write through runtimed `nbformat`, including metadata,
  attachments, outputs, execution counts, and cell IDs where supported by the
  model.
- v4 notebooks plus upgrade paths for legacy and v3 notebooks.
- Single-file, directory, and incremental manifest workflows.
- Atomic workflow output replacement, SHA-256 staleness checks, `_toc.yml`
  settings, and a compatibility `transform-md` binary name.

The current public API is centered on `markdown_to_notebook`,
`script_to_notebook`, `notebook_to_markdown`, `notebook_to_script`,
`notebook_to_json`, `transform_file`, and manifest helpers. The CLI currently
accepts one source file, a Markdown directory, or a workflow manifest/config.

The current implementation does **not** yet provide the complete Jupytext
format matrix, Jinja templates, the complete nbconvert preprocessor catalog,
multi-pair configuration, or notebook execution. External Pandoc, LaTeX,
PDF, Reveal.js, and kernel adapters remain optional future work.

## Compatibility contract

### Notebook model

Use runtimed `nbformat` as the canonical in-memory model unless a required
feature cannot be represented there. Extend the model only with an explicit
compatibility note and round-trip tests.

The transform pipeline must:

- Preserve unknown notebook metadata and cell metadata when the target format
  can carry them.
- Preserve attachments, outputs, execution counts, execution metadata, IDs,
  MIME bundles, and transient fields according to the selected format's rules.
- Keep notebook JSON valid and deterministic without treating key ordering as
  semantic notebook content.
- Validate notebook version and cell structure at input boundaries.
- Report the cell number and field path for user-facing validation failures.
- Make lossy conversions explicit in diagnostics and test fixtures.

### Text formats

Each text format needs a registered descriptor containing:

- Canonical format name and aliases.
- File extensions and language.
- Cell marker grammar.
- Notebook metadata encoding.
- Cell metadata encoding.
- Attachment and output policy.
- Newline, encoding, and final-newline policy.
- Whether round-trip, one-way export, or paired synchronization is supported.

Do not infer format behavior from a file extension alone when a Jupytext
configuration, metadata field, or explicit CLI option provides a stronger
signal.

### CLI and API

The library API should remain usable without filesystem access. File and
workflow operations should be thin adapters over in-memory conversion and
export primitives.

CLI behavior should converge on the documented nbconvert/Jupytext conventions
where they are compatible with existing `nbconvertrs` workflows:

```text
nbconvertrs [OPTIONS] INPUT...
nbconvertrs --to FORMAT INPUT...
nbconvertrs --from FORMAT --to FORMAT INPUT...
nbconvertrs --sync INPUT...
nbconvertrs --execute INPUT...
```

Existing `--output`, `--outdir`, `--out-format`, `--indir`, `--config`,
`--manifest`, and `--dry-run` spellings remain supported. New spelling aliases
must not change existing defaults or output paths.

## Feature matrix

The matrix is a target inventory, not a claim that all rows are currently
implemented.

| Area | Jupytext target | nbconvert target | Planned result |
| --- | --- | --- | --- |
| Notebook JSON | Read/write v4, upgrade older versions | Notebook input and notebook output | Shared validated model and deterministic writer |
| Markdown | Markdown, MyST, Quarto, Pandoc-oriented variants | Markdown export | Format descriptors plus explicit flavor options |
| Scripts | `py:percent`, `py:light`, R, Julia, MATLAB, and other comment-prefix languages | Executable script export | Shared script parser/writer with language registry |
| Cell metadata | YAML, colon options, marker options, notebook metadata | Cell tags and execution metadata | Loss-aware metadata codec |
| Attachments | Markdown attachment references and notebook attachments | Extract or inline resources | Resource manager with configurable policies |
| Pairing | `.ipynb` plus one or more text files | Not a primary nbconvert feature | Pair configuration, sync, precedence, and conflict reporting |
| Conversion | CLI and Python API conversion | Exporter API and CLI conversion | Rust `Converter` and `Exporter` traits |
| Preprocessing | Limited format-specific transforms | Built-in and custom preprocessors | Ordered preprocessor pipeline |
| Templates | Format-specific text writers | Jinja2 templates and inheritance | Template registry and safe Rust/Jinja boundary |
| HTML | Text representation only today | HTML, classic/lab/basic templates | Native HTML exporter and resource policies |
| RST | Not implemented | reStructuredText export | RST exporter, likely with an optional Pandoc bridge initially |
| LaTeX/PDF | Not implemented | LaTeX, PDF, WebPDF | Export contracts plus external-tool adapters |
| Slides | Not implemented | Reveal.js slides | Optional exporter and asset packaging |
| Execution | Not implemented | Execute preprocessor and `--execute` | Kernel protocol adapter, isolated from conversion core |
| Workflow | Manifest, staleness, atomic outputs | Multiple inputs and writers | Unified batch job model |
| Extensions | Rust crate APIs only | Python entry points/config objects | Rust registries, config files, and optional PyO3 bridge |

## Work phases

### Phase 0: Establish conformance infrastructure

**Goal:** make compatibility measurable before expanding the feature surface.

Tasks:

- Pin the upstream Jupytext and nbconvert versions used by the conformance
  harness.
- Build a fixture corpus containing valid notebooks, malformed notebooks,
  every supported text marker, mixed newline styles, Unicode, attachments,
  outputs, rich MIME data, tags, IDs, and unknown metadata.
- Run both upstream tools and `nbconvertrs` over the same fixture matrix.
- Normalize only nondeterministic fields such as timestamps and generated IDs.
- Compare structured notebooks as well as bytes. Report the first differing
  JSON path, text line, resource name, or diagnostic code.
- Add a compatibility ledger with one status per feature: exact, semantic,
  accepted deviation, unsupported, or blocked by an external dependency.
- Add CI jobs for focused Rust tests, upstream Python reference tests, fixture
  parity, and command-line smoke tests.

**Exit criteria:** a new format or exporter cannot be marked complete without a
fixture, a reference result, a Rust result, and a documented difference report.

### Cargo-fuzz scope

The conformance infrastructure includes a separate `cargo-fuzz` package under
`fuzz/`. It is excluded from the production workspace and depends on the
published library boundary rather than private parser helpers. Fuzz targets
must be deterministic, must not execute notebook code, and must not require
Pandoc, a kernel, or a network connection.

The first target set is:

- `markdown_to_notebook`: arbitrary UTF-8-lossy input through Markdown/MyST
  parsing, followed by notebook serialization and a second parse when the
  first parse succeeds.
- `script_to_notebook`: arbitrary input across percent and light Python
  scripts, including marker-like source text, metadata, and mixed newlines.
- `notebook_json`: arbitrary bytes at the notebook JSON boundary, with
  serialization and structural round-trip checks for valid notebooks.
- `export_dispatch`: arbitrary input and format selection through
  `source_to_notebook`, `export_notebook_with_options`, and
  `extract_resources` for every built-in format family.

The seed corpus is organized by behavior rather than by target: valid minimal
documents, nested and unterminated fences, marker-like code, YAML metadata,
legacy/invalid notebook JSON, Unicode and mixed newline input, attachments,
rich MIME outputs, and oversized-but-bounded text. Seed corpus entries are
retained in the nested repository. Crash artifacts remain local and ignored
until a minimized input is promoted to a regression seed; generated coverage
output and build directories are also ignored.

Fuzz targets treat expected parse and unsupported-format errors as normal
results. They fail only on panics, resource exhaustion, or violated semantic
invariants such as invalid notebook JSON after a successful notebook export.
The harness records the target, input format, and normalized diagnostic code
for reproducibility. Local smoke runs use a short `-max_total_time` budget;
scheduled CI runs use a longer budget on the pinned Rust toolchain and publish
minimized crashers. AddressSanitizer and UndefinedBehaviorSanitizer runs are
optional hardening jobs because the core is safe Rust, but they remain useful
for native dependencies and future parser integrations.

**Cargo-fuzz exit criteria:** every target builds with
`cargo +nightly fuzz build`, a short smoke run completes for each target,
checked-in seed inputs exercise each target, and any discovered panic has a
regression test or a documented accepted deviation before the related parity
feature is marked complete.

### Phase 1: Stabilize the conversion core

**Goal:** turn the current functions into a composable conversion pipeline.

Tasks:

- Introduce `NotebookDocument`, `TextDocument`, and `ResourceBundle` concepts
  without breaking the current public functions.
- Add explicit `FormatId`, `InputFormat`, and `OutputFormat` types instead of
  passing format strings through every layer.
- Centralize newline normalization, text encoding, final-newline behavior,
  path handling, and output naming.
- Define structured error codes for parse, validation, unsupported-format,
  resource, execution, template, and writer failures.
- Make the metadata codec preserve unknown fields and distinguish notebook
  metadata from cell metadata.
- Add configurable policies for output retention, attachment retention,
  execution-count retention, and ID generation.
- Add property tests for parse/render/parse invariants and fuzz tests for
  marker parsers and notebook JSON boundaries.

**Exit criteria:** all existing `nbconvertrs` behavior is routed through the
core pipeline, existing CLI workflows remain compatible, and fixture reports
show no new loss of metadata or outputs.

### Phase 2: Complete Jupytext text-format coverage

**Goal:** support the common Jupytext format matrix with predictable aliases.

Tasks:

- Implement a language registry for comment prefixes, file extensions, and
  percent/light marker rules.
- Complete percent and light script behavior, including marker options,
  notebook metadata blocks, cell names, tags, execution metadata, empty cells,
  preambles, and marker-like source text.
- Add Markdown flavors and options for MyST, standard Markdown, Quarto, and
  Pandoc-oriented output. Preserve raw cells and directives without treating
  them as executable code.
- Support Jupytext Markdown cell markers and metadata conventions, including
  format metadata and paired-file metadata.
- Add R Markdown and Quarto-compatible one-way export where their semantics
  differ from ordinary Markdown. Record unsupported execution directives
  rather than silently interpreting them.
- Implement configurable cell splitting policies beyond `m1`, including
  heading-based and marker-based policies.
- Add format detection precedence: explicit option, pairing/config metadata,
  notebook metadata, extension, then content sniffing.
- Add byte fixtures for CRLF, LF, final-newline, indentation, blank cells,
  Unicode, and marker escaping.

**Exit criteria:** the supported Jupytext fixture subset round-trips without
semantic loss, and every unsupported format option has a diagnostic and a
fallback recommendation.

### Phase 3: Pairing, synchronization, and configuration

**Goal:** reach the Jupytext workflow experience, not just its file conversion.

Tasks:

- Define a pairing configuration model equivalent to `jupytext.toml` and
  relevant notebook metadata.
- Support one notebook paired with multiple text representations.
- Implement `--set-formats`, `--sync`, `--pipe`-style filter hooks, and
  conversion aliases while retaining the current CLI.
- Establish source precedence for modified pairs using hashes, timestamps, and
  explicit user selection. Never overwrite a newer pair silently.
- Add conflict diagnostics for divergent notebook and text sources.
- Make pairing updates atomic and recoverable, including partial failure of one
  representation.
- Support stdin/stdout where the selected format can be represented without
  filesystem resources.
- Extend manifests to record pair relationships, format versions, source
  hashes, and transform fingerprints.

**Exit criteria:** a paired notebook can be edited in any supported text form,
then synchronized to all configured pairs with deterministic conflict behavior.

### Phase 4: nbconvert exporter and resource architecture

**Goal:** introduce the abstraction needed for exports beyond notebook/text
conversion.

Tasks:

- Define an `Exporter` trait with in-memory and file-backed entry points:
  `from_notebook`, `from_file`, and `from_path` equivalents.
- Define `ExportResult` with body, MIME type, output extension, metadata, and a
  `ResourceBundle`.
- Define a `Writer` trait for files, directories, memory, stdout, and atomic
  workflow output.
- Implement resource extraction for PNG, JPEG, SVG, PDF, HTML, JavaScript,
  and arbitrary MIME outputs.
- Implement configurable inline-versus-extracted image behavior and stable
  resource naming based on notebook and cell position.
- Support multiple input notebooks and collision-free resource namespaces.
- Add `--stdout`, `--output`, `--output-dir`, overwrite, and dry-run behavior
  consistent with the current CLI and nbconvert use cases.

**Exit criteria:** exporters can be used entirely in memory, and extracted
resources are identical in content and addressable by stable resource keys.

### Phase 5: Built-in nbconvert exporters

**Goal:** deliver the high-value static output formats first.

Implementation order:

1. **Notebook exporter:** normalize or rewrite notebook versions and run
   preprocessors without changing the output format.
2. **Markdown exporter:** render Markdown cells and code cells with explicit
   indentation and attachment policies.
3. **HTML exporter:** provide basic, classic, and lab-like templates as
   versioned assets; support inline or extracted outputs, CSS, JavaScript, and
   sanitized raw HTML policies.
4. **reStructuredText exporter:** provide an initial native implementation for
   common cells and an optional Pandoc-backed compatibility mode.
5. **Executable script exporter:** use the language registry and make magic or
   shell commands explicit in diagnostics.
6. **AsciiDoc exporter:** support the common cell and image subset.
7. **LaTeX exporter:** provide a native template contract and an optional
   external Pandoc/LaTeX toolchain adapter.
8. **PDF exporter:** implement LaTeX and WebPDF adapters with dependency
   detection and actionable errors.
9. **Reveal.js slides:** make this an optional feature with asset discovery,
   speaker notes, fragments, and offline packaging.

For each exporter, define supported templates, MIME type, output extension,
resource behavior, metadata mapping, and accepted deviations from nbconvert.

**Exit criteria:** each completed exporter has golden fixtures, resource tests,
CLI tests, in-memory API tests, and a documented external dependency policy.

### Phase 6: Preprocessors and templates

**Goal:** match nbconvert's composability and customization model.

Tasks:

- Define an ordered `Preprocessor` trait operating on notebook and resources.
- Implement built-ins in small, independently testable crates or modules:
  clear outputs, execute count reset, coalesce streams, convert SVG, extract
  output, regex remove, tag remove, raise-on-error, and strip cells.
- Add tag-based controls for removal, execution, and template selection.
- Define a template loader with built-in templates, filesystem paths, template
  inheritance, and deterministic lookup precedence.
- Use Jinja2-compatible templates through a controlled Rust integration. Avoid
  embedding arbitrary code execution in templates.
- Add configuration files for exporter options and preprocessor ordering.
- Expose extension registration through Rust APIs and, where needed, a PyO3
  bridge for existing Python preprocessors and templates.

**Exit criteria:** a custom exporter can select a template, install a
preprocessor chain, alter resources, and write output through the public API.

### Phase 7: Execution and kernel integration

**Goal:** provide nbconvert-compatible execution without coupling it to the
static conversion core.

Tasks:

- Define an execution backend trait independent of exporters and preprocessors.
- Integrate a Jupyter kernel protocol client using the runtimed ecosystem or a
  documented adapter.
- Implement execution options: kernel name, timeout, startup timeout,
  working directory, environment, allow-errors, error-cell policy, and
  interrupt behavior.
- Capture streams, display data, rich outputs, execution counts, and errors
  with the same notebook semantics as nbconvert.
- Add an isolated process mode and resource limits for untrusted notebooks.
- Make execution opt-in. Static conversion must never execute code implicitly.
- Add deterministic execution fixtures where kernels are available and mocked
  protocol tests where they are not.

**Exit criteria:** `--execute` produces equivalent notebook outputs for the
supported kernels, execution failures are actionable, and static exports remain
safe and deterministic when execution is disabled.

### Phase 8: Extension ecosystem and production hardening

**Goal:** make the parity surface maintainable for downstream projects.

Tasks:

- Add stable versioned traits for formats, exporters, preprocessors, templates,
  writers, execution backends, and configuration providers.
- Add registries and optional dynamic discovery for installed Rust crates.
- Add a PyO3 compatibility layer for Python callers that need existing
  Jupytext or nbconvert plugins.
- Document security boundaries for raw HTML, templates, shell commands,
  external converters, and kernel execution.
- Add cancellation, progress events, structured logging, and machine-readable
  diagnostics.
- Add performance benchmarks for large notebooks, many outputs, and batch
  workflows. Track memory use during resource extraction.
- Add migration guides from current `nbconvertrs`, Jupytext, and nbconvert CLI
  invocations.
- Publish a compatibility table per release and keep the conformance harness
  running against supported upstream versions.

**Exit criteria:** downstream applications can select a stable API, diagnose
unsupported behavior, and upgrade without depending on internal modules.

## Cross-cutting test strategy

### Unit and property tests

- Parser tests for every marker, delimiter, metadata form, newline mode, and
  malformed-input diagnostic.
- Serializer tests for deterministic output, field preservation, and lossy
  format policies.
- Property tests for supported parse/render round trips.
- Fuzz tests for notebook JSON, YAML metadata, HTML markers, script markers,
  templates, and resource names. The cargo-fuzz scope and target contracts are
  defined in Phase 0; pure-Rust property tests remain appropriate for small,
  fast invariants that should run on every commit.

### Reference and golden tests

- Run the same fixtures through pinned Jupytext and nbconvert versions.
- Compare normalized notebook structures, rendered bodies, MIME types, output
  paths, resources, and diagnostics.
- Keep golden files for HTML, Markdown, RST, LaTeX, scripts, slides, and
  notebook outputs.
- Include negative fixtures: invalid JSON, invalid metadata, duplicate IDs,
  unsupported MIME types, missing files, invalid templates, timeout, and kernel
  errors.

### CLI and workflow tests

- Test stdin/stdout, one input, multiple inputs, directory recursion, output
  naming, overwrite, dry-run, and atomic replacement.
- Test pairing conflicts and interrupted multi-output synchronization.
- Test manifest staleness when source content, configuration, format versions,
  or output files change.
- Test external dependency discovery with both installed and missing tools.

### Quality gates

A feature is complete only when:

- The public API and CLI behavior are documented.
- Reference and negative fixtures exist.
- The feature has focused unit tests and at least one integration test.
- Metadata, attachments, outputs, and resources have an explicit retention
  policy.
- Errors are structured, actionable, and stable enough for automation.
- Performance and security implications are recorded.
- The compatibility ledger marks the feature as exact, semantic, or an
  accepted deviation.

## Dependency and risk policy

### External tools

Pandoc, LaTeX, Chromium/Playwright, Reveal.js, and Jupyter kernels should be
optional adapters. The core crate must compile and perform static notebook/text
transforms without them. Missing tools must be detected before an output is
partially written.

### Python compatibility

A PyO3 bridge is a compatibility escape hatch, not a prerequisite for the Rust
core. Python plugins may be delegated to Python while native equivalents are
preferred when declared and available.

### Security

Raw HTML, JavaScript, templates, shell commands, external converters, and
notebook execution are separate trust boundaries. Each must have an explicit
allow/deny configuration, clear defaults, and tests proving that static
conversion does not execute code.

### Performance

Avoid loading all batch inputs into memory at once. Stream files where the
format permits it, retain resources only as long as required, and benchmark
large outputs before adding caching or parallelism. Any cache key must include
the source hash, format, configuration, template version, and relevant
external-tool versions.

## Milestones

| Milestone | Scope | Completion signal |
| --- | --- | --- |
| M0 | Conformance harness | Reference comparison, compatibility ledger, and cargo-fuzz smoke targets run in CI |
| M1 | Conversion core | Typed formats, resource model, diagnostics, and invariant tests |
| M2 | Jupytext formats | Common Markdown/script formats and language registry pass fixtures |
| M3 | Pairing and sync | Multi-pair configuration and conflict-safe synchronization work |
| M4 | Export foundation | Exporter, preprocessor, resources, and writer traits are public |
| M5 | Static exporters | Notebook, Markdown, HTML, RST, script, and AsciiDoc exports work |
| M6 | Templates and preprocessors | Custom template and preprocessor pipeline is supported |
| M7 | Execution | Opt-in kernel execution matches supported nbconvert cases |
| M8 | Ecosystem | Stable extension APIs, bridges, security docs, and release ledger |

## Decisions to make before implementation

1. Which Jupytext and nbconvert upstream versions define the first compatibility
   release?
2. Is exact byte parity required for any format, or is normalized semantic
   parity sufficient for all generated notebook JSON?
3. Should template compatibility prioritize Jinja2 syntax through a Rust engine,
   Python delegation, or a dual-mode implementation?
4. Which external tools are acceptable in CI for LaTeX, PDF, WebPDF, Pandoc, and
   Reveal.js?
5. Which kernels are in scope for the first execution milestone?
6. Should pairing and synchronization live in `nbconvertrs` or in a separate
   crate that depends on the conversion core?
7. What raw HTML and execution defaults are appropriate for sustainablefactory,
   DocIndex, and general-purpose users?

## References

- [Jupytext repository and format overview](https://github.com/jupytext/jupytext)
- [Jupytext documentation](https://jupytext.readthedocs.io/)
- [nbconvert command-line usage](https://nbconvert.readthedocs.io/en/latest/usage.html)
- [nbconvert library and exporter model](https://nbconvert.readthedocs.io/en/latest/nbconvert_library.html)
- [nbformat documentation](https://nbformat.readthedocs.io/)
