//! Jupytext-compatible notebook and text transforms.
//!
//! The crate provides three layers of functionality:
//!
//! - typed format discovery through [`FormatId`] and [`LanguageSpec`];
//! - in-memory conversion through [`Exporter`] and the `*_to_*` functions; and
//! - filesystem workflows through [`transform_file`], [`sync_pair`], and the
//!   manifest helpers.
//!
//! Conversion is deliberately non-executing. Code cells are parsed and
//! rendered as source text; kernel execution and rich-output extraction remain
//! separate compatibility milestones. The [`TransformOptions`] value controls
//! parsing behavior without requiring filesystem access.
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nbformat::Notebook;
use nbformat::v4::{Cell, CellId, CellMetadata, Notebook as NotebookV4, Output};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Errors returned by parsing, conversion, export, and workflow operations.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// An input or output path could not be read, created, or replaced.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The notebook model rejected the input or could not be serialized.
    #[error("notebook error: {0}")]
    Notebook(#[from] nbformat::NotebookError),
    /// A JSON value could not be parsed or serialized.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// A source document or workflow setting is malformed.
    #[error("invalid transform configuration: {0}")]
    Configuration(String),
    /// The requested format is not registered by this crate.
    #[error("unsupported output format {0:?}")]
    UnsupportedFormat(String),
}

/// Stable categories for errors returned by the conversion pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Filesystem access failed.
    Io,
    /// Notebook parsing, upgrading, or validation failed.
    Notebook,
    /// JSON serialization or deserialization failed.
    Json,
    /// Input or configuration was invalid.
    Configuration,
    /// The requested format is not supported.
    UnsupportedFormat,
}

impl TransformError {
    /// Return a machine-readable category for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Io(_) => ErrorCode::Io,
            Self::Notebook(_) => ErrorCode::Notebook,
            Self::Json(_) => ErrorCode::Json,
            Self::Configuration(_) => ErrorCode::Configuration,
            Self::UnsupportedFormat(_) => ErrorCode::UnsupportedFormat,
        }
    }
}

/// A normalized format requested by a caller or detected from a path.
///
/// Use [`FormatId::parse`] at API boundaries. The parser accepts canonical
/// names and aliases such as `markdown`, `notebook`, and `py:percent`, then
/// stores a single canonical representation for dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FormatId {
    /// MyST-compatible Markdown text.
    Markdown,
    /// Jupyter notebook JSON (`.ipynb`).
    Notebook,
    /// Static HTML source rendering.
    Html,
    /// One-way reStructuredText source rendering.
    Rst,
    /// One-way AsciiDoc source rendering.
    AsciiDoc,
    /// Quarto-flavored Markdown source rendering.
    Quarto,
    /// Pandoc-oriented Markdown source rendering.
    Pandoc,
    /// A Jupytext-style script with a language-specific comment prefix.
    Script {
        /// Canonical name of the script language.
        language: String,
        /// Cell-marker convention used by the script.
        kind: ScriptKind,
    },
}

/// Input-format spelling retained as an explicit API concept.
pub type InputFormat = FormatId;

/// Output-format spelling retained as an explicit API concept.
pub type OutputFormat = FormatId;

/// The cell-marker convention used by a script format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptKind {
    /// A script whose cells begin with markers such as `# %%`.
    Percent,
    /// A script whose cells are delimited by markers such as `# +` and `# -`.
    Light,
}

/// Static information used to parse and render a script language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageSpec {
    /// Canonical language name used in [`FormatId::Script`].
    pub name: &'static str,
    /// Case-insensitive file extensions accepted for this language.
    pub extensions: &'static [&'static str],
    /// Comment prefix used for cell markers and Markdown source lines.
    pub comment_prefix: &'static str,
}

const LANGUAGE_SPECS: &[LanguageSpec] = &[
    LanguageSpec {
        name: "python",
        extensions: &["py"],
        comment_prefix: "#",
    },
    LanguageSpec {
        name: "r",
        extensions: &["R", "r"],
        comment_prefix: "#",
    },
    LanguageSpec {
        name: "julia",
        extensions: &["jl"],
        comment_prefix: "#",
    },
    LanguageSpec {
        name: "matlab",
        extensions: &["m"],
        comment_prefix: "%",
    },
    LanguageSpec {
        name: "javascript",
        extensions: &["js"],
        comment_prefix: "//",
    },
    LanguageSpec {
        name: "typescript",
        extensions: &["ts"],
        comment_prefix: "//",
    },
    LanguageSpec {
        name: "ruby",
        extensions: &["rb"],
        comment_prefix: "#",
    },
    LanguageSpec {
        name: "shell",
        extensions: &["sh", "bash"],
        comment_prefix: "#",
    },
    LanguageSpec {
        name: "rust",
        extensions: &["rs"],
        comment_prefix: "//",
    },
    LanguageSpec {
        name: "sql",
        extensions: &["sql"],
        comment_prefix: "--",
    },
];

/// Return the registered script languages understood by the text pipeline.
///
/// The returned slice is static and sorted by the crate's built-in registry,
/// so callers may use it for completion or configuration validation without
/// allocating or holding a lock.
pub fn language_specs() -> &'static [LanguageSpec] {
    LANGUAGE_SPECS
}

/// A format descriptor suitable for discovery and UI/configuration code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatDescriptor {
    /// Canonical format name accepted by [`FormatId::parse`].
    pub name: &'static str,
    /// Additional names accepted as aliases.
    pub aliases: &'static [&'static str],
    /// File extensions associated with the format.
    pub extensions: &'static [&'static str],
    /// Whether conversion is intended to preserve notebook semantics in both
    /// directions.
    pub round_trip: bool,
}

const FORMAT_DESCRIPTORS: &[FormatDescriptor] = &[
    FormatDescriptor {
        name: "myst",
        aliases: &["md", "markdown"],
        extensions: &["md", "markdown", "myst.md"],
        round_trip: true,
    },
    FormatDescriptor {
        name: "ipynb",
        aliases: &["notebook"],
        extensions: &["ipynb"],
        round_trip: true,
    },
    FormatDescriptor {
        name: "html",
        aliases: &[],
        extensions: &["html"],
        round_trip: false,
    },
    FormatDescriptor {
        name: "rst",
        aliases: &["rest"],
        extensions: &["rst", "rest"],
        round_trip: false,
    },
    FormatDescriptor {
        name: "asciidoc",
        aliases: &["adoc"],
        extensions: &["adoc", "asciidoc"],
        round_trip: false,
    },
    FormatDescriptor {
        name: "quarto",
        aliases: &["qmd"],
        extensions: &["qmd"],
        round_trip: false,
    },
    FormatDescriptor {
        name: "pandoc",
        aliases: &[],
        extensions: &["md"],
        round_trip: false,
    },
];

/// Return descriptors for the built-in non-language-specific formats.
///
/// Script formats are described separately by [`language_specs`], because a
/// script format combines a language and a [`ScriptKind`].
pub fn format_descriptors() -> &'static [FormatDescriptor] {
    FORMAT_DESCRIPTORS
}

impl FormatId {
    /// Parse a Jupytext-style format name such as `myst` or `py:percent`.
    pub fn parse(value: &str) -> Result<Self, TransformError> {
        match value.to_ascii_lowercase().as_str() {
            "myst" | "md" | "markdown" => Ok(Self::Markdown),
            "ipynb" | "notebook" => Ok(Self::Notebook),
            "html" => Ok(Self::Html),
            "rst" | "rest" => Ok(Self::Rst),
            "asciidoc" | "adoc" => Ok(Self::AsciiDoc),
            "quarto" | "qmd" => Ok(Self::Quarto),
            "pandoc" => Ok(Self::Pandoc),
            _ => {
                let Some((language, kind)) = value.split_once(':') else {
                    return Err(TransformError::UnsupportedFormat(value.into()));
                };
                let language = language_spec(language)
                    .ok_or_else(|| TransformError::UnsupportedFormat(value.into()))?;
                let kind = match kind.to_ascii_lowercase().as_str() {
                    "percent" => ScriptKind::Percent,
                    "light" => ScriptKind::Light,
                    _ => return Err(TransformError::UnsupportedFormat(value.into())),
                };
                Ok(Self::Script {
                    language: language.name.into(),
                    kind,
                })
            }
        }
    }

    /// Return the canonical CLI/configuration spelling for this format.
    pub fn canonical_name(&self) -> String {
        match self {
            Self::Markdown => "myst".into(),
            Self::Notebook => "ipynb".into(),
            Self::Html => "html".into(),
            Self::Rst => "rst".into(),
            Self::AsciiDoc => "asciidoc".into(),
            Self::Quarto => "quarto".into(),
            Self::Pandoc => "pandoc".into(),
            Self::Script { language, kind } => format!(
                "{}:{}",
                language,
                match kind {
                    ScriptKind::Percent => "percent",
                    ScriptKind::Light => "light",
                }
            ),
        }
    }

    fn output_suffix(&self) -> String {
        match self {
            Self::Markdown => "myst.md".into(),
            Self::Notebook => "ipynb".into(),
            Self::Html => "html".into(),
            Self::Rst => "rst".into(),
            Self::AsciiDoc => "adoc".into(),
            Self::Quarto => "qmd".into(),
            Self::Pandoc => "md".into(),
            Self::Script { language, .. } => language_spec(language)
                .and_then(|spec| spec.extensions.first().copied())
                .unwrap_or(language)
                .into(),
        }
    }
}

fn language_spec(value: &str) -> Option<&'static LanguageSpec> {
    LANGUAGE_SPECS.iter().find(|spec| {
        spec.name.eq_ignore_ascii_case(value)
            || spec
                .extensions
                .iter()
                .any(|extension| extension.eq_ignore_ascii_case(value))
    })
}

/// Output resources collected by an exporter without writing to disk.
///
/// The current built-in text exporters leave this bundle empty. It is part of
/// the public result now so future image, JavaScript, and template exporters
/// can add resources without changing the exporter contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceBundle {
    /// Named binary resources, such as extracted images.
    pub outputs: BTreeMap<String, Vec<u8>>,
    /// String-keyed exporter metadata and configuration results.
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// A notebook plus resources collected during conversion.
#[derive(Debug, Clone)]
pub struct NotebookDocument {
    /// Canonical notebook model.
    pub notebook: NotebookV4,
    /// Resources associated with the notebook.
    pub resources: ResourceBundle,
}

impl NotebookDocument {
    /// Create a document with an empty resource bundle.
    pub fn new(notebook: NotebookV4) -> Self {
        Self {
            notebook,
            resources: ResourceBundle::default(),
        }
    }
}

/// Text content entering or leaving the conversion pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDocument {
    /// Text body, normalized to UTF-8.
    pub body: String,
    /// Format that produced or is expected to consume the body.
    pub format: FormatId,
    /// Resources referenced by the body.
    pub resources: ResourceBundle,
}

/// Policies controlling which notebook data is retained during export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    /// Collect binary display outputs and cell attachments.
    pub extract_resources: bool,
    /// Keep code-cell outputs in source-oriented export processing.
    pub retain_outputs: bool,
    /// Keep execution counts in source-oriented export processing.
    pub retain_execution_counts: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            extract_resources: true,
            retain_outputs: true,
            retain_execution_counts: true,
        }
    }
}

/// The body and resources produced by an in-memory export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    /// The rendered document or serialized notebook body.
    pub body: String,
    /// MIME type for [`ExportResult::body`].
    pub mime_type: String,
    /// File extension to use when writing the body.
    pub output_extension: String,
    /// Supporting resources produced during the export.
    pub resources: ResourceBundle,
}

/// In-memory exporter contract shared by text and future rendered exporters.
pub trait Exporter {
    /// Return the normalized format implemented by this exporter.
    fn format(&self) -> &FormatId;
    /// Export one notebook without reading or writing files.
    fn export(&self, notebook: &NotebookV4) -> Result<ExportResult, TransformError>;
}

/// Built-in exporter for notebook JSON, MyST Markdown, scripts, and static
/// HTML.
#[derive(Debug, Clone)]
pub struct BasicExporter {
    format: FormatId,
    options: ExportOptions,
}

impl BasicExporter {
    /// Construct an exporter for a normalized format.
    pub fn new(format: FormatId) -> Self {
        Self {
            format,
            options: ExportOptions::default(),
        }
    }

    /// Construct an exporter with explicit output retention and resource policies.
    pub fn with_options(format: FormatId, options: ExportOptions) -> Self {
        Self { format, options }
    }
}

impl Exporter for BasicExporter {
    fn format(&self) -> &FormatId {
        &self.format
    }

    fn export(&self, notebook: &NotebookV4) -> Result<ExportResult, TransformError> {
        let resources = if self.options.extract_resources {
            extract_resources(notebook)
        } else {
            ResourceBundle::default()
        };
        let mut notebook = notebook.clone();
        if !self.options.retain_outputs || !self.options.retain_execution_counts {
            for cell in &mut notebook.cells {
                if let Cell::Code {
                    execution_count,
                    outputs,
                    ..
                } = cell
                {
                    if !self.options.retain_outputs {
                        outputs.clear();
                    }
                    if !self.options.retain_execution_counts {
                        *execution_count = None;
                    }
                }
            }
        }
        let (body, mime_type) = match &self.format {
            FormatId::Markdown => (notebook_to_markdown(&notebook), "text/markdown"),
            FormatId::Notebook => (notebook_to_json(&notebook)?, "application/x-ipynb+json"),
            FormatId::Html => (notebook_to_html(&notebook), "text/html"),
            FormatId::Rst => (notebook_to_rst(&notebook), "text/x-rst"),
            FormatId::AsciiDoc => (notebook_to_asciidoc(&notebook), "text/asciidoc"),
            FormatId::Quarto => (notebook_to_markdown(&notebook), "text/markdown"),
            FormatId::Pandoc => (notebook_to_markdown(&notebook), "text/markdown"),
            FormatId::Script { language, kind } => (
                notebook_to_script(
                    &notebook,
                    match kind {
                        ScriptKind::Percent => "percent",
                        ScriptKind::Light => "light",
                    },
                    language,
                ),
                "text/plain",
            ),
        };
        Ok(ExportResult {
            body,
            mime_type: mime_type.into(),
            output_extension: self.format.output_suffix(),
            resources,
        })
    }
}

/// Export a notebook using a canonical format name or supported alias.
///
/// This is the convenience form of [`BasicExporter`] for callers that receive
/// format names from configuration or command-line input.
pub fn export_notebook(
    notebook: &NotebookV4,
    format: &str,
) -> Result<ExportResult, TransformError> {
    BasicExporter::new(FormatId::parse(format)?).export(notebook)
}

/// Export a notebook with explicit retention and resource policies.
pub fn export_notebook_with_options(
    notebook: &NotebookV4,
    format: &str,
    options: ExportOptions,
) -> Result<ExportResult, TransformError> {
    BasicExporter::with_options(FormatId::parse(format)?, options).export(notebook)
}

/// Convert a notebook document to a text document using a registered exporter.
pub trait Converter {
    /// Convert without accessing the filesystem or executing notebook code.
    fn convert(
        &self,
        input: &NotebookDocument,
        output_format: FormatId,
    ) -> Result<TextDocument, TransformError>;
}

/// Default converter backed by [`BasicExporter`].
#[derive(Debug, Clone, Copy, Default)]
pub struct BasicConverter;

impl Converter for BasicConverter {
    fn convert(
        &self,
        input: &NotebookDocument,
        output_format: FormatId,
    ) -> Result<TextDocument, TransformError> {
        let result = BasicExporter::new(output_format.clone()).export(&input.notebook)?;
        Ok(TextDocument {
            body: result.body,
            format: output_format,
            resources: result.resources,
        })
    }
}

/// Run a notebook through an ordered preprocessor pipeline before exporting.
pub fn export_notebook_with_pipeline(
    notebook: &NotebookV4,
    format: &str,
    pipeline: &PreprocessorPipeline,
) -> Result<ExportResult, TransformError> {
    let mut processed = notebook.clone();
    let mut resources = extract_resources(&processed);
    pipeline.process(&mut processed, &mut resources)?;
    let mut result = export_notebook(&processed, format)?;
    result.resources = resources;
    Ok(result)
}

/// A file writer for export bodies and extracted resources.
#[derive(Debug, Clone, Copy)]
pub struct FileWriter {
    /// Replace existing output files when true.
    pub overwrite: bool,
    /// Write through sibling temporary files when true.
    pub atomic: bool,
}

impl Default for FileWriter {
    fn default() -> Self {
        Self {
            overwrite: true,
            atomic: true,
        }
    }
}

impl FileWriter {
    /// Create a writer with explicit overwrite and atomic-write behavior.
    pub fn new(overwrite: bool, atomic: bool) -> Self {
        Self { overwrite, atomic }
    }
}

/// Contract for writing an in-memory export to a destination base path.
pub trait Writer {
    /// Write the primary body and resources, returning the primary output path.
    fn write(&self, result: &ExportResult, output_base: &Path) -> Result<PathBuf, TransformError>;
}

impl Writer for FileWriter {
    fn write(&self, result: &ExportResult, output_base: &Path) -> Result<PathBuf, TransformError> {
        let output = output_base.with_extension(&result.output_extension);
        if !self.overwrite && output.exists() {
            return Err(TransformError::Configuration(format!(
                "output already exists: {}",
                output.display()
            )));
        }
        if self.atomic {
            atomic_write(&output, result.body.as_bytes())?;
        } else {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&output, &result.body)?;
        }
        if !result.resources.outputs.is_empty() {
            let resource_root = output
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("resources");
            fs::create_dir_all(&resource_root)?;
            for (name, contents) in &result.resources.outputs {
                let relative = safe_resource_path(name);
                let path = resource_root.join(relative);
                if self.atomic {
                    atomic_write(&path, contents)?;
                } else {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, contents)?;
                }
            }
        }
        Ok(output)
    }
}

/// Write an export through the supplied writer.
pub fn write_export<W: Writer>(
    writer: &W,
    result: &ExportResult,
    output_base: impl AsRef<Path>,
) -> Result<PathBuf, TransformError> {
    writer.write(result, output_base.as_ref())
}

/// Ordered notebook transformation hook used by exporters and workflows.
pub trait Preprocessor {
    /// Stable name used in diagnostics and configuration.
    fn name(&self) -> &str;
    /// Mutate the notebook and its resources before export.
    fn process(
        &self,
        notebook: &mut NotebookV4,
        resources: &mut ResourceBundle,
    ) -> Result<(), TransformError>;
}

/// An ordered collection of preprocessors.
#[derive(Default)]
pub struct PreprocessorPipeline {
    preprocessors: Vec<Box<dyn Preprocessor>>,
}

impl PreprocessorPipeline {
    /// Create an empty preprocessor pipeline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a preprocessor and return the updated pipeline.
    pub fn push<P: Preprocessor + 'static>(mut self, preprocessor: P) -> Self {
        self.preprocessors.push(Box::new(preprocessor));
        self
    }

    /// Run every preprocessor in registration order.
    pub fn process(
        &self,
        notebook: &mut NotebookV4,
        resources: &mut ResourceBundle,
    ) -> Result<(), TransformError> {
        for preprocessor in &self.preprocessors {
            preprocessor.process(notebook, resources).map_err(|error| {
                TransformError::Configuration(format!(
                    "preprocessor {} failed: {error}",
                    preprocessor.name()
                ))
            })?;
        }
        Ok(())
    }
}

/// Remove all outputs from code cells while retaining their source.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClearOutputs;

impl Preprocessor for ClearOutputs {
    fn name(&self) -> &str {
        "clear_outputs"
    }

    fn process(
        &self,
        notebook: &mut NotebookV4,
        resources: &mut ResourceBundle,
    ) -> Result<(), TransformError> {
        for cell in &mut notebook.cells {
            if let Cell::Code { outputs, .. } = cell {
                outputs.clear();
            }
        }
        resources.outputs.clear();
        resources.metadata.clear();
        Ok(())
    }
}

/// Reset code-cell execution counts without removing outputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResetExecutionCounts;

impl Preprocessor for ResetExecutionCounts {
    fn name(&self) -> &str {
        "reset_execution_counts"
    }

    fn process(
        &self,
        notebook: &mut NotebookV4,
        _resources: &mut ResourceBundle,
    ) -> Result<(), TransformError> {
        for cell in &mut notebook.cells {
            if let Cell::Code {
                execution_count, ..
            } = cell
            {
                *execution_count = None;
            }
        }
        Ok(())
    }
}

/// Merge adjacent stream outputs with the same stream name.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoalesceStreams;

impl Preprocessor for CoalesceStreams {
    fn name(&self) -> &str {
        "coalesce_streams"
    }

    fn process(
        &self,
        notebook: &mut NotebookV4,
        _resources: &mut ResourceBundle,
    ) -> Result<(), TransformError> {
        for cell in &mut notebook.cells {
            let Cell::Code { outputs, .. } = cell else {
                continue;
            };
            let mut coalesced = Vec::with_capacity(outputs.len());
            for output in outputs.drain(..) {
                if let (
                    Some(Output::Stream {
                        name: existing_name,
                        text: existing_text,
                    }),
                    Output::Stream { name, text },
                ) = (coalesced.last_mut(), &output)
                {
                    if existing_name == name {
                        existing_text.0.push_str(&text.0);
                        continue;
                    }
                }
                coalesced.push(output);
            }
            *outputs = coalesced;
        }
        Ok(())
    }
}

/// Remove cells containing any configured tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveTaggedCells {
    /// Tags that cause a cell to be removed.
    pub tags: Vec<String>,
}

impl Preprocessor for RemoveTaggedCells {
    fn name(&self) -> &str {
        "remove_tagged_cells"
    }

    fn process(
        &self,
        notebook: &mut NotebookV4,
        _resources: &mut ResourceBundle,
    ) -> Result<(), TransformError> {
        notebook.cells.retain(|cell| {
            !cell.metadata().tags.as_ref().is_some_and(|cell_tags| {
                cell_tags
                    .iter()
                    .any(|tag| self.tags.iter().any(|wanted| wanted == tag))
            })
        });
        Ok(())
    }
}

/// Options controlling parsing and conversion behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransformOptions {
    /// Split Markdown cells before level-one headings when set to `m1`.
    pub cell_split: Option<String>,
}

/// A filesystem output created by [`transform_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformOutput {
    /// Path of the output file.
    pub path: PathBuf,
    /// Format name supplied by the caller.
    pub format: String,
}

/// Per-source state stored in an incremental transform manifest.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManifestFile {
    /// Path relative to [`TransformManifest::source_root`].
    pub source: String,
    /// SHA-256 hash of the source at the last scan.
    pub sha256: String,
    /// Source size recorded by the workflow scanner.
    #[serde(default)]
    pub size: u64,
    /// Workflow tags associated with the source.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Output format names mapped to their target paths.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    /// Hash of the source used for the last successful transform.
    #[serde(default)]
    pub transformed_sha256: Option<String>,
    /// Configuration fingerprint used for the last successful transform.
    #[serde(default)]
    pub transformed_fingerprint: Option<String>,
    /// Human-readable workflow state.
    #[serde(default)]
    pub status: String,
}

/// Incremental workflow configuration and source records.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransformManifest {
    /// Serialized manifest schema version.
    #[serde(default = "manifest_version")]
    pub version: u32,
    /// Root directory containing source files.
    pub source_root: PathBuf,
    /// Directory used for final output paths.
    pub output_dir: PathBuf,
    /// Optional directory for temporary atomic-transform files.
    #[serde(default)]
    pub temp_dir: Option<PathBuf>,
    /// Output format names used when a record does not provide its own list.
    #[serde(default)]
    pub output_formats: Vec<String>,
    /// Configuration fingerprint used for invalidation.
    #[serde(default)]
    pub transform_fingerprint: String,
    /// Source records keyed by workflow-relative name.
    #[serde(default)]
    pub files: BTreeMap<String, ManifestFile>,
    /// Summary from the last completed transform.
    #[serde(default)]
    pub last_transform: Option<TransformSummary>,
}

/// Counts produced by an incremental workflow operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransformSummary {
    /// Number of records transformed.
    pub transformed: usize,
    /// Number of records that were already current.
    pub skipped: usize,
}

/// Settings loaded from a sustainablefactory `_toc.yml` workflow file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowConfig {
    /// Manifest path resolved relative to the configuration file.
    pub manifest: PathBuf,
    /// Formats requested by the workflow.
    pub output_formats: Vec<String>,
    /// Optional Markdown cell-splitting mode.
    pub cell_split: Option<String>,
}

fn manifest_version() -> u32 {
    1
}

/// Convert a Jupytext-style Markdown document into an nbformat v4 notebook.
///
/// Markdown outside recognized regions becomes a Markdown cell. Fenced code,
/// raw-cell directives, YAML front matter, and supported HTML regions are
/// preserved according to the format rules documented in the package README.
/// The function never executes code.
pub fn markdown_to_notebook(
    source: &str,
    options: &TransformOptions,
) -> Result<NotebookV4, TransformError> {
    let lines = source.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = lines.split_inclusive('\n').collect();
    let mut cells = Vec::new();
    let mut markdown = Vec::new();
    let mut index = 0;
    let mut line_index;
    let mut notebook_metadata = nbformat::v4::Metadata::default();

    let (front_matter, body_start) = parse_front_matter(&lines)?;
    if let Some(metadata) = front_matter {
        notebook_metadata = metadata;
    }

    let flush_markdown = |cells: &mut Vec<Cell>, markdown: &mut Vec<String>, index: &mut usize| {
        let content = markdown.concat();
        if !content.trim().is_empty() {
            cells.push(Cell::Markdown {
                id: cell_id(*index),
                metadata: CellMetadata::default(),
                source: split_source(&content),
                attachments: None,
            });
            *index += 1;
        }
        markdown.clear();
    };

    line_index = body_start;
    while line_index < lines.len() {
        let line = lines[line_index];
        if let Some((cell_type, metadata)) = region_start(line)? {
            flush_markdown(&mut cells, &mut markdown, &mut index);
            line_index += 1;
            let mut body = Vec::new();
            while line_index < lines.len() && !region_end(lines[line_index], cell_type) {
                body.push(lines[line_index]);
                line_index += 1;
            }
            if line_index == lines.len() {
                return Err(TransformError::Configuration(
                    "unterminated HTML cell region".into(),
                ));
            }
            let source = body.concat();
            let metadata = metadata.unwrap_or_default();
            let id = cell_id(index);
            if cell_type == CellTypeTag::Raw {
                cells.push(Cell::Raw {
                    id,
                    metadata,
                    source: split_source(&source),
                });
            } else {
                cells.push(Cell::Markdown {
                    id,
                    metadata,
                    source: split_source(&source),
                    attachments: None,
                });
            }
            index += 1;
            line_index += 1;
            continue;
        }
        if let Some((fence, info)) = fence_start(line) {
            flush_markdown(&mut cells, &mut markdown, &mut index);
            line_index += 1;
            let mut body = Vec::new();
            while line_index < lines.len() && !fence_end(lines[line_index], fence) {
                body.push(lines[line_index]);
                line_index += 1;
            }
            if line_index == lines.len() {
                return Err(TransformError::Configuration(
                    "unterminated Markdown code fence".into(),
                ));
            }
            let body = body.concat();
            let info = info.trim();
            let raw = info.starts_with("{raw-cell}") || info == "raw";
            let language = info
                .strip_prefix("{code-cell}")
                .map(str::trim)
                .or_else(|| {
                    info.strip_prefix("{code-cell ")
                        .map(|value| value.trim_end_matches('}').trim())
                })
                .unwrap_or(info)
                .split_whitespace()
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("python");
            let (body, mut metadata) = parse_cell_options(&body)?;
            metadata.additional.insert(
                "language".into(),
                serde_json::Value::String(language.into()),
            );
            let id = cell_id(index);
            if raw {
                cells.push(Cell::Raw {
                    id,
                    metadata,
                    source: split_source(&body),
                });
            } else {
                cells.push(Cell::Code {
                    id,
                    metadata,
                    execution_count: None,
                    source: split_source(&body),
                    outputs: Vec::new(),
                });
            }
            index += 1;
            line_index += 1;
            continue;
        }

        if options.cell_split.as_deref() == Some("m1")
            && line.strip_suffix('\n').unwrap_or(line).starts_with("# ")
            && !markdown.is_empty()
        {
            flush_markdown(&mut cells, &mut markdown, &mut index);
        }
        markdown.push(line.to_owned());
        line_index += 1;
    }
    flush_markdown(&mut cells, &mut markdown, &mut index);

    Ok(NotebookV4 {
        metadata: notebook_metadata,
        nbformat: 4,
        nbformat_minor: 5,
        cells,
    })
}

/// Convert a Jupytext percent or light script into an nbformat v4 notebook.
///
/// `format` must be `percent` or `light`; `default_language` selects the
/// comment prefix and language metadata. Use [`FormatId::parse`] when the
/// format is supplied as a combined name such as `py:percent`.
pub fn script_to_notebook(
    source: &str,
    format: &str,
    default_language: &str,
) -> Result<NotebookV4, TransformError> {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split_inclusive('\n').collect();
    let comment_prefix = script_comment_prefix(default_language);
    let (front_matter, body_start) = parse_script_front_matter(&lines, comment_prefix)?;
    let percent = format.eq_ignore_ascii_case("percent");
    let mut cells = Vec::new();
    let mut current: Option<(CellTypeTag, CellMetadata, Vec<&str>)> = None;
    let mut preamble: Vec<&str> = Vec::new();

    for line in &lines[body_start..] {
        let marker = if percent {
            script_percent_marker(line, comment_prefix)
        } else {
            script_light_marker(line, comment_prefix)
        };
        match marker {
            Some(ScriptMarker::Start(options)) => {
                if let Some(cell) = current.take() {
                    push_script_cell(&mut cells, cell, default_language, comment_prefix);
                } else if preamble.iter().any(|line| !line.trim().is_empty()) {
                    push_script_cell(
                        &mut cells,
                        (
                            CellTypeTag::Code,
                            CellMetadata::default(),
                            std::mem::take(&mut preamble),
                        ),
                        default_language,
                        comment_prefix,
                    );
                }
                current = Some((options.0, options.1, Vec::new()));
            }
            Some(ScriptMarker::End) if !percent => {
                if let Some(cell) = current.take() {
                    push_script_cell(&mut cells, cell, default_language, comment_prefix);
                }
            }
            None => {
                let markdown_cell = current
                    .as_ref()
                    .is_some_and(|(cell_type, _, _)| *cell_type == CellTypeTag::Markdown);
                if !percent
                    && markdown_cell
                    && !line.trim().is_empty()
                    && !line.trim_start().starts_with(comment_prefix)
                {
                    let cell = current.take().expect("current cell exists");
                    push_script_cell(&mut cells, cell, default_language, comment_prefix);
                    current = Some((CellTypeTag::Code, CellMetadata::default(), vec![line]));
                } else if let Some((_, _, body)) = &mut current {
                    body.push(line);
                } else {
                    if !percent && line.trim_start().starts_with(comment_prefix) {
                        let mut body = std::mem::take(&mut preamble);
                        body.push(line);
                        current = Some((CellTypeTag::Markdown, CellMetadata::default(), body));
                    } else {
                        preamble.push(line);
                    }
                }
            }
            Some(ScriptMarker::End) => {}
        }
    }
    if let Some(cell) = current.take() {
        push_script_cell(&mut cells, cell, default_language, comment_prefix);
    } else if preamble.iter().any(|line| !line.trim().is_empty()) {
        push_script_cell(
            &mut cells,
            (CellTypeTag::Code, CellMetadata::default(), preamble),
            default_language,
            comment_prefix,
        );
    }

    Ok(NotebookV4 {
        metadata: front_matter.unwrap_or_default(),
        nbformat: 4,
        nbformat_minor: 5,
        cells,
    })
}

/// Render an nbformat v4 notebook in Jupytext-compatible MyST Markdown form.
pub fn notebook_to_markdown(notebook: &NotebookV4) -> String {
    let mut output = String::new();
    let metadata = serde_yaml::to_string(&notebook.metadata).unwrap();
    if metadata.trim() != "{}" {
        output.push_str("---\n");
        output.push_str(&metadata);
        output.push_str("---\n\n");
    }
    for (index, cell) in notebook.cells.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            output.push('\n');
        }
        match cell {
            Cell::Markdown {
                metadata, source, ..
            } => {
                if metadata_is_empty(metadata) {
                    output.push_str(&source.concat());
                } else {
                    output.push_str(&html_region_start("region", metadata));
                    output.push('\n');
                    output.push_str(&source.concat());
                    output.push_str("<!-- #endregion -->\n");
                }
            }
            Cell::Raw {
                metadata, source, ..
            } => {
                if metadata_is_empty(metadata) {
                    output.push_str("```{raw-cell}\n");
                    output.push_str(&source.concat());
                    output.push_str("```\n");
                } else {
                    output.push_str(&html_region_start("raw", metadata));
                    output.push('\n');
                    output.push_str(&source.concat());
                    output.push_str("<!-- #endraw -->\n");
                }
            }
            Cell::Code {
                metadata, source, ..
            } => {
                let language = metadata
                    .additional
                    .get("language")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("python");
                output.push_str("```");
                output.push_str(language);
                output.push('\n');
                append_myst_metadata(&mut output, metadata);
                output.push_str(&source.concat());
                output.push_str("```\n");
            }
        }
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

/// Render a notebook as a Jupytext percent or light script.
///
/// Markdown cells are comment-prefixed and code/raw cells retain their source
/// text. The language controls both the comment prefix and output language
/// metadata.
pub fn notebook_to_script(notebook: &NotebookV4, format: &str, language: &str) -> String {
    let percent = format.eq_ignore_ascii_case("percent");
    let comment_prefix = script_comment_prefix(language);
    let marker = if percent {
        format!("{comment_prefix} %%")
    } else {
        format!("{comment_prefix} +")
    };
    let end_marker = format!("{comment_prefix} -");
    let mut output = String::new();
    let metadata = serde_yaml::to_string(&notebook.metadata).unwrap();
    if metadata.trim() != "{}" {
        for line in metadata.lines() {
            output.push_str(comment_prefix);
            output.push(' ');
            output.push_str(line);
            output.push('\n');
        }
        output.insert_str(0, &format!("{comment_prefix} ---\n"));
        output.push_str(&format!("{comment_prefix} ---\n\n"));
    }
    for (index, cell) in notebook.cells.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let (cell_type, metadata, source) = match cell {
            Cell::Markdown {
                metadata, source, ..
            } => (CellTypeTag::Markdown, metadata, source),
            Cell::Raw {
                metadata, source, ..
            } => (CellTypeTag::Raw, metadata, source),
            Cell::Code {
                metadata, source, ..
            } => (CellTypeTag::Code, metadata, source),
        };
        output.push_str(&marker);
        if cell_type == CellTypeTag::Markdown {
            output.push_str(" [markdown]");
        } else if cell_type == CellTypeTag::Raw {
            output.push_str(" [raw]");
        } else if let Ok(metadata) = metadata_json(metadata) {
            if !metadata.is_empty() {
                output.push(' ');
                output.push_str(&metadata);
            }
        }
        output.push('\n');
        if cell_type == CellTypeTag::Markdown {
            for line in source.concat().lines() {
                output.push_str(comment_prefix);
                output.push(' ');
                output.push_str(line);
                output.push('\n');
            }
        } else {
            output.push_str(&source.concat());
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
        if !percent {
            output.push_str(&end_marker);
            output.push('\n');
        }
    }
    let _ = language;
    output
}

/// Render a notebook as reStructuredText with native code blocks.
pub fn notebook_to_rst(notebook: &NotebookV4) -> String {
    let mut output = String::new();
    for (index, cell) in notebook.cells.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            output.push('\n');
        }
        match cell {
            Cell::Markdown { source, .. } | Cell::Raw { source, .. } => {
                output.push_str(&source.concat());
            }
            Cell::Code {
                metadata, source, ..
            } => {
                let language = metadata
                    .additional
                    .get("language")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("text");
                output.push_str(".. code-block:: ");
                output.push_str(language);
                output.push_str("\n\n");
                for line in source.concat().lines() {
                    output.push_str("   ");
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }
    if !output.is_empty() && !output.ends_with("\n\n") {
        output.push('\n');
    }
    output
}

/// Render a notebook as AsciiDoc with source blocks.
pub fn notebook_to_asciidoc(notebook: &NotebookV4) -> String {
    let mut output = String::new();
    for (index, cell) in notebook.cells.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            output.push('\n');
        }
        match cell {
            Cell::Markdown { source, .. } | Cell::Raw { source, .. } => {
                output.push_str(&source.concat());
            }
            Cell::Code {
                metadata, source, ..
            } => {
                let language = metadata
                    .additional
                    .get("language")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("text");
                output.push_str("[source,");
                output.push_str(language);
                output.push_str("]\n----\n");
                output.push_str(&source.concat());
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("----\n");
            }
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

/// Render notebook source into a deterministic, dependency-free HTML document.
///
/// Cell source is escaped and placed in stable indexed sections. This is a
/// static source renderer: it does not execute cells or render rich MIME
/// outputs.
pub fn notebook_to_html(notebook: &NotebookV4) -> String {
    let mut output = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Notebook</title>\n</head>\n<body>\n",
    );
    for (index, cell) in notebook.cells.iter().enumerate() {
        match cell {
            Cell::Markdown { source, .. } => {
                output.push_str(&format!(
                    "<section class=\"cell markdown-cell\" data-cell-index=\"{index}\"><pre>{}</pre></section>\n",
                    html_escape(&source.concat())
                ));
            }
            Cell::Raw { source, .. } => {
                output.push_str(&format!(
                    "<section class=\"cell raw-cell\" data-cell-index=\"{index}\"><pre>{}</pre></section>\n",
                    html_escape(&source.concat())
                ));
            }
            Cell::Code {
                metadata, source, ..
            } => {
                let language = metadata
                    .additional
                    .get("language")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("python");
                output.push_str(&format!(
                    "<section class=\"cell code-cell\" data-cell-index=\"{index}\"><pre><code class=\"language-{}\">{}</code></pre></section>\n",
                    html_escape(language),
                    html_escape(&source.concat())
                ));
            }
        }
    }
    output.push_str("</body>\n</html>\n");
    output
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Serialize a notebook using runtimed's nbformat-compatible writer.
pub fn notebook_to_json(notebook: &NotebookV4) -> Result<String, TransformError> {
    Ok(nbformat::serialize_notebook(&Notebook::V4(
        notebook.clone(),
    ))?)
}

/// Parse source text into the canonical notebook model.
///
/// `format` must identify a supported input format. Markdown, Quarto, and
/// Pandoc-oriented Markdown currently share the Markdown parser; RST and
/// AsciiDoc are output-only until native parsers are added.
pub fn source_to_notebook(
    source: &str,
    format: &str,
    options: &TransformOptions,
) -> Result<NotebookV4, TransformError> {
    match FormatId::parse(format)? {
        FormatId::Notebook => notebook_from_json(source),
        FormatId::Script { language, kind } => script_to_notebook(
            source,
            match kind {
                ScriptKind::Percent => "percent",
                ScriptKind::Light => "light",
            },
            &language,
        ),
        FormatId::Markdown | FormatId::Quarto | FormatId::Pandoc => {
            markdown_to_notebook(source, options)
        }
        FormatId::Html | FormatId::Rst | FormatId::AsciiDoc => Err(TransformError::Configuration(
            "the selected format is output-only".into(),
        )),
    }
}

/// Extract binary notebook attachments and rich display data into stable keys.
pub fn extract_resources(notebook: &NotebookV4) -> ResourceBundle {
    let mut resources = ResourceBundle::default();
    for (cell_index, cell) in notebook.cells.iter().enumerate() {
        let Ok(value) = serde_json::to_value(cell) else {
            continue;
        };
        if let Some(attachments) = value
            .get("attachments")
            .and_then(serde_json::Value::as_object)
        {
            for (name, data) in attachments {
                collect_mime_resources(
                    &mut resources,
                    data,
                    format!(
                        "cell-{cell_index}/attachment-{}",
                        safe_resource_path(name).display()
                    ),
                );
            }
        }
        if let Some(outputs) = value.get("outputs").and_then(serde_json::Value::as_array) {
            for (output_index, output) in outputs.iter().enumerate() {
                if let Some(data) = output.get("data") {
                    collect_mime_resources(
                        &mut resources,
                        data,
                        format!("cell-{cell_index}/output-{output_index}"),
                    );
                }
            }
        }
    }
    resources
}

fn collect_mime_resources(
    resources: &mut ResourceBundle,
    value: &serde_json::Value,
    prefix: String,
) {
    let Some(data) = value.as_object() else {
        return;
    };
    for (mime, payload) in data {
        let Some(payload) = payload.as_str() else {
            continue;
        };
        let Some(extension) = resource_extension(mime) else {
            continue;
        };
        let contents = if mime.starts_with("image/") || mime == "application/pdf" {
            decode_base64(payload).unwrap_or_else(|| payload.as_bytes().to_vec())
        } else {
            payload.as_bytes().to_vec()
        };
        let name = format!("{prefix}.{extension}");
        resources.outputs.insert(name.clone(), contents);
        resources
            .metadata
            .insert(name, serde_json::Value::String(mime.clone()));
    }
}

fn resource_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/svg+xml" => Some("svg"),
        "application/pdf" => Some("pdf"),
        "text/html" => Some("html"),
        "application/javascript" | "text/javascript" => Some("js"),
        _ => None,
    }
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

fn safe_resource_path(name: &str) -> PathBuf {
    Path::new(name)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect()
}

/// Convert one source file to the requested output files.
///
/// The input format is inferred from the extension and script markers. Use
/// [`transform_file_with_input_format`] when inference is not sufficient.
pub fn transform_file(
    source: impl AsRef<Path>,
    output_base: impl AsRef<Path>,
    formats: &[String],
    options: &TransformOptions,
) -> Result<Vec<TransformOutput>, TransformError> {
    transform_file_with_input_format(source, output_base, formats, options, None)
}

/// Transform a file while allowing callers to override extension-based input detection.
///
/// `input_format` accepts the same names as [`FormatId::parse`]. Output files
/// are written beneath `output_base` using each format's canonical extension.
pub fn transform_file_with_input_format(
    source: impl AsRef<Path>,
    output_base: impl AsRef<Path>,
    formats: &[String],
    options: &TransformOptions,
    input_format: Option<&str>,
) -> Result<Vec<TransformOutput>, TransformError> {
    let source = source.as_ref();
    let source_text = fs::read_to_string(source)?;
    let input_format = input_format
        .map(FormatId::parse)
        .transpose()?
        .unwrap_or_else(|| detect_input_format(source, &source_text));
    let notebook = source_to_notebook(&source_text, &input_format.canonical_name(), options)?;
    let output_base = output_base.as_ref();
    if let Some(parent) = output_base.parent() {
        fs::create_dir_all(parent)?;
    }
    let parsed_formats = formats
        .iter()
        .map(|format| FormatId::parse(format))
        .collect::<Result<Vec<_>, _>>()?;
    let mut outputs = Vec::new();
    let writer = FileWriter::default();
    for (format, parsed_format) in formats.iter().zip(parsed_formats) {
        let exported = BasicExporter::new(parsed_format).export(&notebook)?;
        let path = write_export(&writer, &exported, output_base)?;
        outputs.push(TransformOutput {
            path,
            format: format.clone(),
        });
    }
    Ok(outputs)
}

/// Detect a source format using the extension and, for scripts, marker content.
///
/// Unknown extensions default to [`FormatId::Markdown`]. This function only
/// detects the format; it does not parse or validate the source body.
pub fn detect_input_format(path: &Path, source: &str) -> FormatId {
    let extension = path.extension().and_then(|value| value.to_str());
    if extension.is_some_and(|value| value.eq_ignore_ascii_case("ipynb")) {
        return FormatId::Notebook;
    }
    if extension.is_some_and(|value| value.eq_ignore_ascii_case("qmd")) {
        return FormatId::Quarto;
    }
    if extension.is_some_and(|value| {
        value.eq_ignore_ascii_case("rst") || value.eq_ignore_ascii_case("rest")
    }) {
        return FormatId::Rst;
    }
    if extension.is_some_and(|value| {
        value.eq_ignore_ascii_case("adoc") || value.eq_ignore_ascii_case("asciidoc")
    }) {
        return FormatId::AsciiDoc;
    }
    if let Some(spec) = extension.and_then(language_spec) {
        let kind = if source
            .lines()
            .any(|line| script_percent_marker(line, spec.comment_prefix).is_some())
        {
            ScriptKind::Percent
        } else {
            ScriptKind::Light
        };
        return FormatId::Script {
            language: spec.name.into(),
            kind,
        };
    }
    FormatId::Markdown
}

fn notebook_from_json(source: &str) -> Result<NotebookV4, TransformError> {
    match nbformat::parse_notebook(source)? {
        Notebook::V4(notebook) => Ok(notebook),
        Notebook::V4QuirksMode(notebook) => Ok(notebook.repair()),
        Notebook::Legacy(notebook) => nbformat::upgrade_legacy_notebook(notebook)
            .map_err(|error| TransformError::Configuration(error.to_string())),
        Notebook::V3(notebook) => nbformat::upgrade_v3_notebook(notebook)
            .map_err(|error| TransformError::Configuration(error.to_string())),
        _ => Err(TransformError::Configuration(
            "unsupported notebook variant".into(),
        )),
    }
}

/// Direction selected while reconciling a notebook/text pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// The notebook was authoritative and regenerated the text file.
    NotebookToText,
    /// The text file was authoritative and regenerated the notebook.
    TextToNotebook,
    /// Both files already represented the same canonical text.
    Unchanged,
}

/// Result of synchronizing one notebook/text pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncResult {
    /// Direction selected by the synchronization operation.
    pub direction: SyncDirection,
    /// Notebook path in the pair.
    pub notebook: PathBuf,
    /// Text path in the pair.
    pub text: PathBuf,
}

/// Synchronize a notebook and one Jupytext-compatible text representation.
///
/// When both files differ, the newer file is authoritative. Equal timestamps
/// are treated as a conflict so an automated workflow cannot overwrite edits.
/// Missing files are created from the file that exists. Writes use a temporary
/// sibling followed by an atomic rename.
pub fn sync_pair(
    notebook_path: impl AsRef<Path>,
    text_path: impl AsRef<Path>,
    text_format: &str,
    options: &TransformOptions,
) -> Result<SyncResult, TransformError> {
    let notebook_path = notebook_path.as_ref().to_owned();
    let text_path = text_path.as_ref().to_owned();
    let text_format_id = FormatId::parse(text_format)?;
    if matches!(
        text_format_id,
        FormatId::Notebook
            | FormatId::Html
            | FormatId::Rst
            | FormatId::AsciiDoc
            | FormatId::Quarto
            | FormatId::Pandoc
    ) {
        return Err(TransformError::Configuration(
            "sync text format must be a round-trip Markdown or script format".into(),
        ));
    }

    let notebook_exists = notebook_path.is_file();
    let text_exists = text_path.is_file();
    match (notebook_exists, text_exists) {
        (false, false) => {
            return Err(TransformError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "sync pair has no notebook or text source",
            )));
        }
        (true, false) => {
            let notebook = notebook_from_json(&fs::read_to_string(&notebook_path)?)?;
            let exported = BasicExporter::new(text_format_id).export(&notebook)?;
            atomic_write(&text_path, exported.body.as_bytes())?;
            return Ok(SyncResult {
                direction: SyncDirection::NotebookToText,
                notebook: notebook_path,
                text: text_path,
            });
        }
        (false, true) => {
            let source = fs::read_to_string(&text_path)?;
            let notebook = match &text_format_id {
                FormatId::Markdown => markdown_to_notebook(&source, options)?,
                FormatId::Script { language, kind } => script_to_notebook(
                    &source,
                    match kind {
                        ScriptKind::Percent => "percent",
                        ScriptKind::Light => "light",
                    },
                    language,
                )?,
                FormatId::Notebook
                | FormatId::Html
                | FormatId::Rst
                | FormatId::AsciiDoc
                | FormatId::Quarto
                | FormatId::Pandoc => unreachable!(),
            };
            atomic_write(&notebook_path, notebook_to_json(&notebook)?.as_bytes())?;
            return Ok(SyncResult {
                direction: SyncDirection::TextToNotebook,
                notebook: notebook_path,
                text: text_path,
            });
        }
        (true, true) => {}
    }

    let notebook = notebook_from_json(&fs::read_to_string(&notebook_path)?)?;
    let text_source = fs::read_to_string(&text_path)?;
    let text_notebook = match &text_format_id {
        FormatId::Markdown => markdown_to_notebook(&text_source, options)?,
        FormatId::Script { language, kind } => script_to_notebook(
            &text_source,
            match kind {
                ScriptKind::Percent => "percent",
                ScriptKind::Light => "light",
            },
            language,
        )?,
        FormatId::Notebook
        | FormatId::Html
        | FormatId::Rst
        | FormatId::AsciiDoc
        | FormatId::Quarto
        | FormatId::Pandoc => unreachable!(),
    };
    let canonical_text = BasicExporter::new(text_format_id.clone())
        .export(&notebook)?
        .body;
    if canonical_text == text_source {
        return Ok(SyncResult {
            direction: SyncDirection::Unchanged,
            notebook: notebook_path,
            text: text_path,
        });
    }

    let notebook_modified = fs::metadata(&notebook_path)?.modified()?;
    let text_modified = fs::metadata(&text_path)?.modified()?;
    if notebook_modified == text_modified {
        return Err(TransformError::Configuration(format!(
            "sync conflict: both sources changed and have the same timestamp ({})",
            notebook_path.display()
        )));
    }
    if notebook_modified > text_modified {
        let exported = BasicExporter::new(text_format_id).export(&notebook)?;
        atomic_write(&text_path, exported.body.as_bytes())?;
        Ok(SyncResult {
            direction: SyncDirection::NotebookToText,
            notebook: notebook_path,
            text: text_path,
        })
    } else {
        atomic_write(&notebook_path, notebook_to_json(&text_notebook)?.as_bytes())?;
        Ok(SyncResult {
            direction: SyncDirection::TextToNotebook,
            notebook: notebook_path,
            text: text_path,
        })
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), TransformError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("output"),
        std::process::id()
    ));
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Load workflow settings from the sustainablefactory `_toc.yml` shape.
///
/// The loader searches nested mappings for `output_formats` and resolves a
/// relative `manifest` path against the configuration file's directory.
pub fn load_workflow_config(path: impl AsRef<Path>) -> Result<WorkflowConfig, TransformError> {
    let path = path.as_ref();
    let source = fs::read_to_string(path)?;
    let value: serde_yaml::Value = serde_yaml::from_str(&source).map_err(|error| {
        TransformError::Configuration(format!("invalid workflow YAML: {error}"))
    })?;
    let settings = find_workflow_mapping(&value).ok_or_else(|| {
        TransformError::Configuration("workflow config has no output_formats mapping".into())
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let manifest = settings
        .get("manifest")
        .and_then(serde_yaml::Value::as_str)
        .map(|value| resolve_config_path(parent, value))
        .unwrap_or_else(|| parent.join(".tmp/workflow/chat-manifest.json"));
    let output_formats = settings
        .get("output_formats")
        .and_then(serde_yaml::Value::as_sequence)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| vec!["myst".into(), "ipynb".into()]);
    let cell_split = settings
        .get("transform")
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|transform| transform.get(serde_yaml::Value::String("cell_split".into())))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_owned);
    Ok(WorkflowConfig {
        manifest,
        output_formats,
        cell_split,
    })
}

/// Return manifest records whose source hash, transform fingerprint, or outputs are stale.
///
/// Missing source files count as stale. The returned names are the manifest
/// keys, in map order.
pub fn changed_manifest_files(manifest: &TransformManifest) -> Vec<String> {
    manifest
        .files
        .iter()
        .filter_map(|(name, record)| {
            let source = manifest.source_root.join(&record.source);
            let source_changed = sha256_path(&source)
                .map(|hash| hash != record.sha256)
                .unwrap_or(true);
            let outputs_missing = record
                .outputs
                .values()
                .any(|path| !Path::new(path).is_file());
            if source_changed
                || record.transformed_sha256.as_deref() != Some(record.sha256.as_str())
                || record.transformed_fingerprint.as_deref()
                    != Some(manifest.transform_fingerprint.as_str())
                || outputs_missing
            {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Transform stale manifest records, replacing all outputs atomically after success.
pub fn transform_manifest(
    path: impl AsRef<Path>,
    dry_run: bool,
) -> Result<TransformSummary, TransformError> {
    transform_manifest_with_options(path, dry_run, &TransformOptions::default(), None)
}

/// Transform a manifest using options loaded from the workflow configuration.
pub fn transform_manifest_with_config(
    path: impl AsRef<Path>,
    dry_run: bool,
    config: &WorkflowConfig,
) -> Result<TransformSummary, TransformError> {
    transform_manifest_with_options(
        path,
        dry_run,
        &TransformOptions {
            cell_split: config.cell_split.clone(),
        },
        Some(&config.output_formats),
    )
}

fn transform_manifest_with_options(
    path: impl AsRef<Path>,
    dry_run: bool,
    options: &TransformOptions,
    configured_formats: Option<&[String]>,
) -> Result<TransformSummary, TransformError> {
    let path = path.as_ref();
    let mut manifest: TransformManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
    let changed = changed_manifest_files(&manifest);
    let summary = TransformSummary {
        transformed: changed.len(),
        skipped: manifest.files.len().saturating_sub(changed.len()),
    };
    if dry_run {
        return Ok(summary);
    }
    let formats = if !manifest.output_formats.is_empty() {
        manifest.output_formats.clone()
    } else if let Some(configured_formats) = configured_formats {
        configured_formats.to_vec()
    } else {
        vec!["myst".into(), "ipynb".into()]
    };
    let temporary_root = manifest
        .temp_dir
        .clone()
        .unwrap_or_else(|| manifest.output_dir.join(".nbconvertrs-tmp"));
    fs::create_dir_all(&temporary_root)?;
    for name in &changed {
        let record = manifest.files.get(name).cloned().ok_or_else(|| {
            TransformError::Configuration(format!("missing manifest record {name}"))
        })?;
        let source = manifest.source_root.join(&record.source);
        let temporary_dir = unique_temp_dir(&temporary_root)?;
        let output_base = temporary_dir.join(source.file_stem().unwrap_or_default());
        let result = transform_file(&source, &output_base, &formats, options);
        match result {
            Ok(outputs) => {
                for output in outputs {
                    let target = record.outputs.get(&output.format).ok_or_else(|| {
                        TransformError::Configuration(format!(
                            "manifest has no output path for format {}",
                            output.format
                        ))
                    })?;
                    let target = PathBuf::from(target);
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(output.path, target)?;
                }
                let current_hash = sha256_path(&source)?;
                let entry = manifest
                    .files
                    .get_mut(name)
                    .expect("record was cloned above");
                entry.transformed_sha256 = Some(current_hash);
                entry.transformed_fingerprint = Some(manifest.transform_fingerprint.clone());
                entry.status = "transformed".into();
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&temporary_dir);
                return Err(error);
            }
        }
        fs::remove_dir_all(temporary_dir)?;
    }
    manifest.last_transform = Some(summary.clone());
    write_manifest(path, &manifest)?;
    Ok(summary)
}

fn write_manifest(path: &Path, manifest: &TransformManifest) -> Result<(), TransformError> {
    let temporary = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("manifest")
    ));
    fs::write(&temporary, serde_json::to_string_pretty(manifest)? + "\n")?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn sha256_path(path: &Path) -> Result<String, std::io::Error> {
    let bytes = fs::read(path)?;
    let mut digest = Sha256::new();
    digest.update(bytes);
    Ok(format!("{:x}", digest.finalize()))
}

fn unique_temp_dir(root: &Path) -> Result<PathBuf, std::io::Error> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!("transform-{}-{timestamp}", std::process::id()));
    fs::create_dir(&path)?;
    Ok(path)
}

fn resolve_config_path(parent: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        parent.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn find_workflow_mapping(value: &serde_yaml::Value) -> Option<&serde_yaml::Mapping> {
    let mapping = value.as_mapping()?;
    if mapping.contains_key(serde_yaml::Value::String("output_formats".into())) {
        return Some(mapping);
    }
    mapping.values().find_map(find_workflow_mapping)
}

fn cell_id(index: usize) -> CellId {
    CellId::try_from(format!("cell-{index}"))
        .expect("generated notebook cell IDs are valid by construction")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CellTypeTag {
    Code,
    Markdown,
    Raw,
}

enum ScriptMarker {
    Start(Box<(CellTypeTag, CellMetadata)>),
    End,
}

fn parse_front_matter(
    lines: &[&str],
) -> Result<(Option<nbformat::v4::Metadata>, usize), TransformError> {
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Ok((None, 0));
    }
    let Some(end) = lines.iter().skip(1).position(|line| line.trim() == "---") else {
        return Err(TransformError::Configuration(
            "unterminated YAML front matter".into(),
        ));
    };
    let end = end + 1;
    let metadata = metadata_from_yaml(&lines[1..end].concat())?;
    Ok((Some(metadata), end + 1))
}

fn parse_script_front_matter(
    lines: &[&str],
    comment_prefix: &str,
) -> Result<(Option<nbformat::v4::Metadata>, usize), TransformError> {
    let delimiter = format!("{comment_prefix} ---");
    if lines.first().map(|line| line.trim()) != Some(delimiter.as_str()) {
        return Ok((None, 0));
    }
    let Some(end) = lines
        .iter()
        .skip(1)
        .position(|line| line.trim() == delimiter)
    else {
        return Err(TransformError::Configuration(
            "unterminated YAML front matter".into(),
        ));
    };
    let end = end + 1;
    let yaml = lines[1..end]
        .iter()
        .map(|line| {
            line.strip_prefix(&format!("{comment_prefix} "))
                .or_else(|| line.strip_prefix(comment_prefix))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .concat();
    let metadata = metadata_from_yaml(&yaml)?;
    Ok((Some(metadata), end + 1))
}

fn metadata_from_yaml(yaml: &str) -> Result<nbformat::v4::Metadata, TransformError> {
    let mut value = yaml_value(yaml)?;
    if let Some(jupyter) = value.get("jupyter").cloned() {
        if jupyter.is_object() {
            value = jupyter;
        }
    }
    serde_json::from_value(value).map_err(|error| {
        TransformError::Configuration(format!("invalid notebook metadata: {error}"))
    })
}

fn yaml_value(yaml: &str) -> Result<serde_json::Value, TransformError> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|error| {
        TransformError::Configuration(format!("invalid YAML metadata: {error}"))
    })?;
    serde_json::to_value(value)
        .map_err(|error| TransformError::Configuration(format!("invalid metadata: {error}")))
}

fn parse_cell_options(source: &str) -> Result<(String, CellMetadata), TransformError> {
    let mut lines = source.split_inclusive('\n').collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) == Some("---") {
        if let Some(end) = lines.iter().skip(1).position(|line| line.trim() == "---") {
            let end = end + 1;
            let metadata = metadata_from_value(yaml_value(&lines[1..end].concat())?)?;
            return Ok((lines.drain(end + 1..).collect(), metadata));
        }
        return Err(TransformError::Configuration(
            "unterminated cell YAML metadata".into(),
        ));
    }
    let mut metadata_lines = Vec::new();
    while let Some(line) = lines.first() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(':') {
            break;
        }
        metadata_lines.push(trimmed.strip_prefix(':').unwrap_or(trimmed));
        lines.remove(0);
    }
    if metadata_lines.is_empty() {
        return Ok((source.to_owned(), CellMetadata::default()));
    }
    let metadata = metadata_from_value(yaml_value(&metadata_lines.concat())?)?;
    Ok((lines.concat(), metadata))
}

fn region_start(line: &str) -> Result<Option<(CellTypeTag, Option<CellMetadata>)>, TransformError> {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("<!-- #") else {
        return Ok(None);
    };
    let Some(rest) = rest.strip_suffix("-->") else {
        return Ok(None);
    };
    let rest = rest.trim();
    let (cell_type, options) = if let Some(options) = rest.strip_prefix("region") {
        (CellTypeTag::Markdown, options.trim())
    } else if let Some(options) = rest.strip_prefix("markdown") {
        (CellTypeTag::Markdown, options.trim())
    } else if let Some(options) = rest.strip_prefix("raw") {
        (CellTypeTag::Raw, options.trim())
    } else {
        return Ok(None);
    };
    let metadata = if options.is_empty() {
        None
    } else {
        let value = serde_json::from_str(options).map_err(|error| {
            TransformError::Configuration(format!("invalid HTML cell metadata: {error}"))
        })?;
        Some(metadata_from_value(value)?)
    };
    Ok(Some((cell_type, metadata)))
}

fn region_end(line: &str, cell_type: CellTypeTag) -> bool {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("<!-- #") else {
        return false;
    };
    let Some(rest) = rest.strip_suffix("-->") else {
        return false;
    };
    let expected = match cell_type {
        CellTypeTag::Markdown => ["endregion", "endmarkdown"],
        CellTypeTag::Raw => ["endraw", "endregion"],
        CellTypeTag::Code => ["", ""],
    };
    expected.into_iter().any(|value| rest.trim() == value)
}

fn metadata_from_value(value: serde_json::Value) -> Result<CellMetadata, TransformError> {
    if !value.is_object() {
        return Err(TransformError::Configuration(
            "cell metadata must be a mapping".into(),
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| TransformError::Configuration(format!("invalid cell metadata: {error}")))
}

fn script_percent_marker(line: &str, comment_prefix: &str) -> Option<ScriptMarker> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(comment_prefix)?.trim_start();
    let rest = rest.strip_prefix("%%")?;
    if !rest.is_empty()
        && !rest.chars().next().is_some_and(char::is_whitespace)
        && !rest.starts_with(['[', '{'])
    {
        return None;
    }
    Some(ScriptMarker::Start(Box::new(parse_script_options(rest))))
}

fn script_light_marker(line: &str, comment_prefix: &str) -> Option<ScriptMarker> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(comment_prefix)?.trim();
    if rest == "-" {
        return Some(ScriptMarker::End);
    }
    let options = rest.strip_prefix('+')?;
    Some(ScriptMarker::Start(Box::new(parse_script_options(options))))
}

fn script_comment_prefix(language: &str) -> &'static str {
    language_spec(language)
        .map(|spec| spec.comment_prefix)
        .unwrap_or("#")
}

fn parse_script_options(options: &str) -> (CellTypeTag, CellMetadata) {
    let options = options.trim();
    if options.eq_ignore_ascii_case("[markdown]") {
        return (CellTypeTag::Markdown, CellMetadata::default());
    }
    if options.eq_ignore_ascii_case("[raw]") {
        return (CellTypeTag::Raw, CellMetadata::default());
    }
    if options.starts_with('{') {
        if let Ok(value) = serde_json::from_str(options) {
            if let Ok(metadata) = metadata_from_value(value) {
                return (CellTypeTag::Code, metadata);
            }
        }
    }
    let mut metadata = CellMetadata::default();
    if !options.is_empty() {
        metadata.name = Some(options.to_owned());
    }
    (CellTypeTag::Code, metadata)
}

fn push_script_cell(
    cells: &mut Vec<Cell>,
    cell: (CellTypeTag, CellMetadata, Vec<&str>),
    default_language: &str,
    comment_prefix: &str,
) {
    let (cell_type, mut metadata, mut body) = cell;
    while body.last().is_some_and(|line| line.trim().is_empty()) {
        body.pop();
    }
    let source = if cell_type == CellTypeTag::Markdown {
        body.iter()
            .map(|line| {
                line.strip_prefix(&format!("{comment_prefix} "))
                    .or_else(|| line.strip_prefix(comment_prefix))
                    .unwrap_or(line)
            })
            .collect::<String>()
    } else {
        body.concat()
    };
    let id = cell_id(cells.len());
    match cell_type {
        CellTypeTag::Markdown => cells.push(Cell::Markdown {
            id,
            metadata,
            source: split_source(&source),
            attachments: None,
        }),
        CellTypeTag::Raw => cells.push(Cell::Raw {
            id,
            metadata,
            source: split_source(&source),
        }),
        CellTypeTag::Code => {
            metadata.additional.insert(
                "language".into(),
                serde_json::Value::String(default_language.into()),
            );
            cells.push(Cell::Code {
                id,
                metadata,
                execution_count: None,
                source: split_source(&source),
                outputs: Vec::new(),
            });
        }
    }
}

fn metadata_is_empty(metadata: &CellMetadata) -> bool {
    serde_json::to_value(metadata)
        .map(|value| value.as_object().is_none_or(|object| object.is_empty()))
        .unwrap_or(true)
}

fn metadata_json(metadata: &CellMetadata) -> Result<String, TransformError> {
    let mut value = serde_json::to_value(metadata)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("language");
    }
    if value.as_object().is_none_or(|object| object.is_empty()) {
        return Ok(String::new());
    }
    Ok(serde_json::to_string(&value)?)
}

fn append_myst_metadata(output: &mut String, metadata: &CellMetadata) {
    let Ok(mut value) = serde_json::to_value(metadata) else {
        return;
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("language");
    }
    if value.as_object().is_none_or(|object| object.is_empty()) {
        return;
    }
    if let Ok(yaml) = serde_yaml::to_string(&value) {
        output.push_str("---\n");
        output.push_str(&yaml);
        output.push_str("---\n");
    }
}

fn html_region_start(kind: &str, metadata: &CellMetadata) -> String {
    let value = serde_json::to_string(metadata).unwrap_or_else(|_| "{}".into());
    if metadata_is_empty(metadata) {
        format!("<!-- #{kind} -->")
    } else {
        format!("<!-- #{kind} {value} -->")
    }
}

fn split_source(source: &str) -> Vec<String> {
    if source.is_empty() {
        return Vec::new();
    }
    source.split_inclusive('\n').map(str::to_owned).collect()
}

fn fence_start(line: &str) -> Option<(char, &str)> {
    let trimmed = line.trim_start();
    let (fence, rest) = if let Some(rest) = trimmed.strip_prefix("```") {
        ('`', rest)
    } else if let Some(rest) = trimmed.strip_prefix("~~~") {
        ('~', rest)
    } else {
        return None;
    };
    Some((fence, rest.trim()))
}

fn fence_end(line: &str, fence: char) -> bool {
    line.trim_start()
        .starts_with(&format!("{fence}{fence}{fence}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown_and_code_cells() {
        let notebook = markdown_to_notebook(
            "# Title\n\nIntro\n\n```python\nprint(1)\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 2);
        assert!(matches!(notebook.cells[0], Cell::Markdown { .. }));
        assert!(matches!(notebook.cells[1], Cell::Code { .. }));
        assert_eq!(notebook.cells[1].id().as_str(), "cell-1");
    }

    #[test]
    fn m1_splits_markdown_cells_at_h1() {
        let notebook = markdown_to_notebook(
            "# One\n\nBody\n\n# Two\n\nMore\n",
            &TransformOptions {
                cell_split: Some("m1".into()),
            },
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 2);
        assert!(notebook.cells[0].source().concat().contains("# One"));
        assert!(notebook.cells[1].source().concat().contains("# Two"));
    }

    #[test]
    fn format_registry_normalizes_aliases_and_scripts() {
        assert_eq!(FormatId::parse("markdown").unwrap(), FormatId::Markdown);
        assert_eq!(FormatId::parse("notebook").unwrap(), FormatId::Notebook);
        assert_eq!(
            FormatId::parse("py:percent").unwrap(),
            FormatId::Script {
                language: "python".into(),
                kind: ScriptKind::Percent,
            }
        );
        assert_eq!(
            FormatId::parse("javascript:light")
                .unwrap()
                .canonical_name(),
            "javascript:light"
        );
        assert!(FormatId::parse("python:unknown").is_err());
        assert!(
            format_descriptors()
                .iter()
                .any(|format| format.name == "myst")
        );
        assert!(
            language_specs()
                .iter()
                .any(|language| language.name == "sql")
        );
    }

    #[test]
    fn detects_registered_script_languages_and_prefixes() {
        let javascript = detect_input_format(
            Path::new("example.js"),
            "// %% [markdown]\n// Title\n// %%\nanswer = 1\n",
        );
        assert_eq!(
            javascript,
            FormatId::Script {
                language: "javascript".into(),
                kind: ScriptKind::Percent,
            }
        );
        let notebook = script_to_notebook(
            "// %% [markdown]\n// Title\n// %%\nanswer = 1\n",
            "percent",
            "javascript",
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 2);
        assert!(notebook_to_script(&notebook, "percent", "javascript").contains("// %%"));
    }

    #[test]
    fn basic_exporter_returns_in_memory_result() {
        let notebook = markdown_to_notebook(
            "# Title\n\n```rust\nlet value = \"<safe>\";\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        let result = export_notebook(&notebook, "markdown").unwrap();
        assert_eq!(result.mime_type, "text/markdown");
        assert_eq!(result.output_extension, "myst.md");
        assert!(result.body.contains("# Title\n"));
        assert!(result.resources.outputs.is_empty());

        let script = export_notebook(&notebook, "py:percent").unwrap();
        assert_eq!(script.mime_type, "text/plain");
        assert_eq!(script.output_extension, "py");
        assert!(script.body.contains("# %%"));

        let html = export_notebook(&notebook, "html").unwrap();
        assert_eq!(html.mime_type, "text/html");
        assert_eq!(html.output_extension, "html");
        assert!(html.body.contains("&lt;safe&gt;"));
    }

    #[test]
    fn export_foundation_supports_formats_resources_and_writers() {
        let notebook = markdown_to_notebook(
            "# Title\n\n```python\nprint(1)\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        let rst = export_notebook(&notebook, "rst").unwrap();
        assert_eq!(rst.mime_type, "text/x-rst");
        assert!(rst.body.contains(".. code-block:: python"));
        let adoc = export_notebook(&notebook, "adoc").unwrap();
        assert_eq!(adoc.output_extension, "adoc");
        assert!(adoc.body.contains("[source,python]"));

        let document = NotebookDocument::new(notebook.clone());
        let converted = BasicConverter.convert(&document, FormatId::Quarto).unwrap();
        assert_eq!(converted.format, FormatId::Quarto);
        assert!(converted.body.contains("# Title"));

        let output = tempfile::tempdir().unwrap();
        let path = write_export(
            &FileWriter::default(),
            &rst,
            output.path().join("nested/note"),
        )
        .unwrap();
        assert_eq!(path, output.path().join("nested/note.rst"));
        assert!(path.is_file());
    }

    #[test]
    fn resource_extraction_and_preprocessors_are_deterministic() {
        let json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"cell-0","metadata":{"tags":["remove"]},"execution_count":3,"source":["print(1)\n"],"outputs":[{"output_type":"display_data","data":{"image/png":"SGVsbG8="},"metadata":{}}]}]}"##;
        let notebook = match nbformat::parse_notebook(json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected v4 notebook"),
        };
        let resources = extract_resources(&notebook);
        assert_eq!(
            resources.outputs.get("cell-0/output-0.png"),
            Some(&b"Hello".to_vec())
        );
        assert_eq!(
            resources.metadata.get("cell-0/output-0.png"),
            Some(&serde_json::Value::String("image/png".into()))
        );
        let pipeline = PreprocessorPipeline::new()
            .push(ClearOutputs)
            .push(ResetExecutionCounts)
            .push(RemoveTaggedCells {
                tags: vec!["remove".into()],
            });
        let result = export_notebook_with_pipeline(&notebook, "ipynb", &pipeline).unwrap();
        assert!(!result.body.contains("execution_count"));
        assert!(!result.body.contains("print(1)"));

        let stream_json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"cell-0","metadata":{},"execution_count":null,"source":["print(1)\n"],"outputs":[{"output_type":"stream","name":"stdout","text":["one\n"]},{"output_type":"stream","name":"stdout","text":["two\n"]}]}]}"##;
        let mut stream_notebook = match nbformat::parse_notebook(stream_json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected v4 notebook"),
        };
        let mut stream_resources = ResourceBundle::default();
        PreprocessorPipeline::new()
            .push(CoalesceStreams)
            .process(&mut stream_notebook, &mut stream_resources)
            .unwrap();
        let stream_output = notebook_to_json(&stream_notebook).unwrap();
        let parsed_stream = match nbformat::parse_notebook(&stream_output).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected v4 notebook"),
        };
        match &parsed_stream.cells[0] {
            Cell::Code { outputs, .. } => {
                assert_eq!(outputs.len(), 1);
                match &outputs[0] {
                    Output::Stream { text, .. } => assert_eq!(text.0, "one\ntwo\n"),
                    _ => panic!("expected stream output"),
                }
            }
            _ => panic!("expected code cell"),
        }

        let file_result = ExportResult {
            body: "body\n".into(),
            mime_type: "text/plain".into(),
            output_extension: "txt".into(),
            resources: ResourceBundle {
                outputs: BTreeMap::from([("../asset.bin".into(), b"asset".to_vec())]),
                metadata: BTreeMap::new(),
            },
        };
        let output = tempfile::tempdir().unwrap();
        let path = write_export(
            &FileWriter::default(),
            &file_result,
            output.path().join("written"),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "body\n");
        assert_eq!(
            fs::read(output.path().join("resources/asset.bin")).unwrap(),
            b"asset"
        );
    }

    #[test]
    fn export_policies_and_error_codes_are_explicit() {
        let error = FormatId::parse("unknown").unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnsupportedFormat);
        let json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"cell-0","metadata":{},"execution_count":3,"source":["value = 1\n"],"outputs":[] }]}"##;
        let notebook = match nbformat::parse_notebook(json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected v4 notebook"),
        };
        let result = export_notebook_with_options(
            &notebook,
            "ipynb",
            ExportOptions {
                extract_resources: false,
                retain_outputs: true,
                retain_execution_counts: false,
            },
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result.body).unwrap();
        assert_eq!(
            value["cells"][0]["execution_count"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn synchronizes_a_pair_without_rewriting_equivalent_content() {
        let directory = tempfile::tempdir().unwrap();
        let notebook_path = directory.path().join("note.ipynb");
        let text_path = directory.path().join("note.md");
        let notebook = markdown_to_notebook("# Title\n", &TransformOptions::default()).unwrap();
        fs::write(&notebook_path, notebook_to_json(&notebook).unwrap()).unwrap();

        let created = sync_pair(
            &notebook_path,
            &text_path,
            "myst",
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(created.direction, SyncDirection::NotebookToText);
        assert_eq!(fs::read_to_string(&text_path).unwrap(), "# Title\n");

        let unchanged = sync_pair(
            &notebook_path,
            &text_path,
            "myst",
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(unchanged.direction, SyncDirection::Unchanged);

        fs::remove_file(&notebook_path).unwrap();
        let reconstructed = sync_pair(
            &notebook_path,
            &text_path,
            "markdown",
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(reconstructed.direction, SyncDirection::TextToNotebook);
        assert!(notebook_path.is_file());
    }

    #[test]
    fn round_trips_notebook_json() {
        let notebook = markdown_to_notebook(
            "Text\n\n```{code-cell} rust\nfn main() {}\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        let json = notebook_to_json(&notebook).unwrap();
        let parsed = nbformat::parse_notebook(&json).unwrap();
        assert!(matches!(parsed, Notebook::V4(_)));
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn rejects_unclosed_fences() {
        let error = markdown_to_notebook("```python\nprint(1)\n", &TransformOptions::default())
            .unwrap_err();
        assert!(error.to_string().contains("unterminated"));
    }

    #[test]
    fn parses_front_matter_myst_metadata_and_html_regions() {
        let notebook = markdown_to_notebook(
            "---\nkernelspec:\n  display_name: Python 3\n  name: python3\n---\n\n<!-- #region {\"tags\":[\"intro\"]} -->\n# Intro\n<!-- #endregion -->\n\n```{code-cell} rust\n---\ntags: [parameters]\nname: setup\n---\nlet answer = 42;\n```\n\n<!-- #raw {\"name\":\"diagram\"} -->\n<diagram />\n<!-- #endraw -->\n",
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(
            notebook.metadata.kernelspec.as_ref().unwrap().name,
            "python3"
        );
        assert_eq!(notebook.cells.len(), 3);
        assert_eq!(
            notebook.cells[0]
                .metadata()
                .tags
                .as_deref()
                .map(|tags| tags.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["intro"])
        );
        assert_eq!(notebook.cells[1].metadata().name.as_deref(), Some("setup"));
        assert_eq!(
            notebook.cells[1]
                .metadata()
                .additional
                .get("language")
                .and_then(serde_json::Value::as_str),
            Some("rust")
        );
        assert!(matches!(notebook.cells[2], Cell::Raw { .. }));
        let rendered = notebook_to_markdown(&notebook);
        assert!(rendered.contains("kernelspec:"));
        assert!(rendered.contains("#region"));
        assert!(rendered.contains("#raw"));
    }

    #[test]
    fn parses_percent_script_cells_and_metadata() {
        let notebook = script_to_notebook(
            "# ---\n# jupyter:\n#   kernelspec:\n#     display_name: Python 3\n#     name: python3\n# ---\n# %% [markdown]\n# # Title\n# Body\n\n# %% {\"tags\":[\"parameters\"]}\nvalue = 1\n",
            "percent",
            "python",
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 2);
        assert_eq!(notebook.cells[0].source().concat(), "# Title\nBody\n");
        assert_eq!(
            notebook.cells[1]
                .metadata()
                .tags
                .as_deref()
                .map(|tags| tags.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["parameters"])
        );
        assert_eq!(
            notebook.metadata.kernelspec.as_ref().unwrap().name,
            "python3"
        );
        let rendered = notebook_to_script(&notebook, "percent", "python");
        assert!(!rendered.contains("\"language\""));
        let reparsed = script_to_notebook(&rendered, "percent", "python").unwrap();
        assert_eq!(reparsed.cells.len(), 2);
        assert_eq!(reparsed.cells[1].source().concat(), "value = 1\n");
    }

    #[test]
    fn parses_light_script_markers_and_raw_cells() {
        let notebook = script_to_notebook(
            "# + {\"name\":\"setup\"}\nvalue = 1\n# -\n# + [markdown]\n# Notes\n# -\n# + [raw]\nraw text\n# -\n",
            "light",
            "python",
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 3);
        assert_eq!(notebook.cells[0].metadata().name.as_deref(), Some("setup"));
        assert_eq!(notebook.cells[1].source().concat(), "Notes\n");
        assert!(matches!(notebook.cells[2], Cell::Raw { .. }));
        let rendered = notebook_to_script(&notebook, "light", "python");
        assert!(rendered.contains("# + [markdown]"));
        assert!(rendered.contains("# -"));
    }

    #[test]
    fn parses_upstream_percent_and_light_fixtures() {
        let percent = script_to_notebook(
            include_str!("../../../../jupytext/tests/unit/data/landing_page/notebook_percent.py"),
            "percent",
            "python",
        )
        .unwrap();
        assert_eq!(percent.cells.len(), 3);
        assert!(matches!(percent.cells[0], Cell::Markdown { .. }));
        assert!(percent.cells[1].source().concat().contains("read_csv"));

        let light = script_to_notebook(
            include_str!("../../../../jupytext/tests/unit/data/landing_page/notebook_light.py"),
            "light",
            "python",
        )
        .unwrap();
        assert_eq!(light.cells.len(), 2);
        assert!(matches!(light.cells[0], Cell::Markdown { .. }));
        assert!(light.cells[1].source().concat().contains("groupby"));
    }

    #[test]
    fn supports_matlab_comment_prefixes() {
        let notebook = script_to_notebook(
            "% %% [markdown]\n% # Title\n\n% %%\nanswer = 42\n",
            "percent",
            "matlab",
        )
        .unwrap();
        assert_eq!(notebook.cells.len(), 2);
        assert_eq!(notebook.cells[0].source().concat(), "# Title\n");
        assert_eq!(notebook.cells[1].source().concat(), "answer = 42\n");
        assert!(notebook_to_script(&notebook, "percent", "matlab").contains("% %%"));
    }

    #[test]
    fn transform_file_supports_notebook_input_and_multiple_formats() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.md");
        let output = directory.path().join("converted");
        fs::write(&source, "# Title\n\n```python\nprint(1)\n```\n").unwrap();
        let results = transform_file(
            &source,
            &output,
            &["ipynb".into(), "py:percent".into()],
            &TransformOptions::default(),
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert!(output.with_extension("ipynb").is_file());
        assert!(output.with_extension("py").is_file());
        let script = fs::read_to_string(output.with_extension("py")).unwrap();
        assert!(script.contains("# %%"));
        assert!(script.contains("print(1)"));
    }

    #[test]
    fn notebook_json_preserves_metadata_and_attachments() {
        let json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{"kernelspec":{"display_name":"Python 3","name":"python3"}},"cells":[{"cell_type":"markdown","id":"cell-0","metadata":{"tags":["keep"]},"source":["![image](attachment:image.png)"],"attachments":{"image.png":{"image/png":"AAAA"}}}]}"##;
        let notebook = match nbformat::parse_notebook(json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected v4 notebook"),
        };
        let serialized = notebook_to_json(&notebook).unwrap();
        assert!(serialized.contains("attachment:image.png"));
        assert!(serialized.contains("image/png"));
        assert!(serialized.contains("python3"));
    }

    #[test]
    fn transform_file_preserves_existing_notebook_json() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.ipynb");
        let output = directory.path().join("copied");
        let json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"cell-0","metadata":{},"execution_count":7,"source":["print(1)\n"],"outputs":[{"name":"stdout","output_type":"stream","text":["1\n"]}]}]}"##;
        fs::write(&source, json).unwrap();
        transform_file(
            &source,
            &output,
            &["ipynb".into()],
            &TransformOptions::default(),
        )
        .unwrap();
        let copied = fs::read_to_string(output.with_extension("ipynb")).unwrap();
        assert!(copied.contains("execution_count"));
        assert!(copied.contains("stdout"));
        assert!(copied.contains("1\\n"));
    }

    #[test]
    fn workflow_manifest_is_incremental_and_supports_dry_run() {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("source");
        let output_dir = directory.path().join("output");
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("note.md");
        fs::write(&source, "# Title\n\n```python\nprint(1)\n```\n").unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let mut outputs = BTreeMap::new();
        outputs.insert(
            "myst".into(),
            output_dir
                .join("note.myst.md")
                .to_string_lossy()
                .into_owned(),
        );
        outputs.insert(
            "ipynb".into(),
            output_dir.join("note.ipynb").to_string_lossy().into_owned(),
        );
        let mut files = BTreeMap::new();
        files.insert(
            "note.md".into(),
            ManifestFile {
                source: "note.md".into(),
                sha256: sha256_path(&source).unwrap(),
                outputs,
                status: "selected".into(),
                ..ManifestFile::default()
            },
        );
        let manifest = TransformManifest {
            source_root,
            output_dir,
            output_formats: Vec::new(),
            transform_fingerprint: "test-fingerprint".into(),
            files,
            ..TransformManifest::default()
        };
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        let config = WorkflowConfig {
            manifest: manifest_path.clone(),
            output_formats: vec!["myst".into(), "ipynb".into()],
            cell_split: None,
        };

        let dry_run = transform_manifest_with_config(&manifest_path, true, &config).unwrap();
        assert_eq!(
            dry_run,
            TransformSummary {
                transformed: 1,
                skipped: 0
            }
        );
        assert!(
            !manifest_path
                .parent()
                .unwrap()
                .join("output/note.ipynb")
                .exists()
        );

        let transformed = transform_manifest_with_config(&manifest_path, false, &config).unwrap();
        assert_eq!(
            transformed,
            TransformSummary {
                transformed: 1,
                skipped: 0
            }
        );
        assert!(
            manifest_path
                .parent()
                .unwrap()
                .join("output/note.ipynb")
                .is_file()
        );
        let saved: TransformManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(saved.files["note.md"].status, "transformed");

        let skipped = transform_manifest_with_config(&manifest_path, false, &config).unwrap();
        assert_eq!(
            skipped,
            TransformSummary {
                transformed: 0,
                skipped: 1
            }
        );
    }

    #[test]
    fn loads_nested_workflow_config_relative_to_toc() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("docs/_toc.yml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "sustainablefactory:\n  chat_sources:\n    output_formats: [myst, ipynb]\n    manifest: ../.tmp/workflow/chat-manifest.json\n    transform:\n      cell_split: m1\n",
        )
        .unwrap();
        let config = load_workflow_config(&config_path).unwrap();
        assert_eq!(config.output_formats, vec!["myst", "ipynb"]);
        assert_eq!(config.cell_split.as_deref(), Some("m1"));
        assert_eq!(
            config.manifest,
            directory.path().join(".tmp/workflow/chat-manifest.json")
        );
    }

    #[test]
    fn covers_parser_errors_and_alternate_cell_writers() {
        assert!(
            markdown_to_notebook("<!-- #region -->\nbody\n", &TransformOptions::default()).is_err()
        );
        assert!(
            markdown_to_notebook("---\nnot: [closed\n---\n", &TransformOptions::default()).is_err()
        );
        assert!(markdown_to_notebook("---\n- list\n---\n", &TransformOptions::default()).is_err());
        assert!(
            markdown_to_notebook(
                "<!-- #region invalid -->\nbody\n<!-- #endregion -->\n",
                &TransformOptions::default()
            )
            .is_err()
        );
        assert!(
            markdown_to_notebook(
                "```python\n---\n- list\n---\ncode\n```\n",
                &TransformOptions::default()
            )
            .is_err()
        );
        assert!(
            markdown_to_notebook(
                "```python\n---\ntags: [one]\ncode\n",
                &TransformOptions::default()
            )
            .is_err()
        );
        assert!(
            markdown_to_notebook(
                "```python\n---\ntags: [one]\n```\n",
                &TransformOptions::default()
            )
            .is_err()
        );
        assert!(markdown_to_notebook("---\nname: value\n", &TransformOptions::default()).is_err());
        assert!(markdown_to_notebook("<!-- regular -->\n", &TransformOptions::default()).is_ok());
        assert!(region_start("<!-- #region").unwrap().is_none());
        assert!(region_start("<!-- #unknown -->").unwrap().is_none());
        assert!(!region_end("<!-- #endregion", CellTypeTag::Markdown));
        assert!(!region_end("<!-- #endmarkdown -->", CellTypeTag::Code));
        assert!(metadata_from_value(serde_json::Value::Null).is_err());
        assert_eq!(
            parse_script_options("{\"name\":\"named\"}")
                .1
                .name
                .as_deref(),
            Some("named")
        );
        assert_eq!(
            parse_script_options("{invalid}").1.name.as_deref(),
            Some("{invalid}")
        );
        let mut language_metadata = CellMetadata::default();
        language_metadata
            .additional
            .insert("language".into(), serde_json::json!("python"));
        let mut rendered = String::new();
        append_myst_metadata(&mut rendered, &language_metadata);
        assert!(rendered.is_empty());
        assert_eq!(
            html_region_start("region", &CellMetadata::default()),
            "<!-- #region -->"
        );

        let notebook = markdown_to_notebook(
            "~~~raw\nraw\n~~~\n\n```python\n:tags: [example]\nprint(1)\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        assert!(matches!(notebook.cells[0], Cell::Raw { .. }));
        assert!(notebook_to_markdown(&notebook).contains("raw"));
        assert!(notebook_to_script(&notebook, "light", "r").contains("# + [raw]"));
        assert!(notebook_to_script(&notebook, "percent", "julia").contains("# %%"));
        assert!(notebook_to_script(&notebook, "light", "matlab").contains("% +"));
        let code_with_metadata = markdown_to_notebook(
            "```python\n:tags: [code]\nvalue = 1\n```\n",
            &TransformOptions::default(),
        )
        .unwrap();
        assert!(notebook_to_script(&code_with_metadata, "light", "python").contains("tags"));
        assert!(
            markdown_to_notebook(
                "<!-- #markdown -->\ntext\n<!-- #endmarkdown -->\n",
                &TransformOptions::default(),
            )
            .is_ok()
        );
        assert!(split_source("").is_empty());
        assert!(
            script_to_notebook("# -\n", "light", "python")
                .unwrap()
                .cells
                .is_empty()
        );

        let raw_json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"raw","metadata":{},"source":"raw"}]}"##;
        let raw = match nbformat::parse_notebook(raw_json).unwrap() {
            Notebook::V4QuirksMode(notebook) => notebook.repair(),
            _ => panic!("expected a repaired v4 notebook"),
        };
        assert!(notebook_to_markdown(&raw).contains("```{raw-cell}"));
        let markdown_json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"markdown","id":"cell-0","metadata":{},"source":"text"}]}"##;
        let markdown = match nbformat::parse_notebook(markdown_json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected a v4 notebook"),
        };
        assert_eq!(notebook_to_markdown(&markdown), "text\n");
        let code_json = r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"code","id":"cell-0","metadata":{},"execution_count":null,"source":"value = 1","outputs":[] }]}"##;
        let code = match nbformat::parse_notebook(code_json).unwrap() {
            Notebook::V4(notebook) => notebook,
            _ => panic!("expected a v4 notebook"),
        };
        assert!(notebook_to_script(&code, "light", "python").contains("value = 1\n"));
    }

    #[test]
    fn covers_script_edge_cases_and_dispatch_formats() {
        assert!(script_to_notebook("# ---\n# broken\n", "percent", "python").is_err());
        assert!(script_to_notebook("# ---\n# - list\n# ---\n", "percent", "python").is_err());
        let light = script_to_notebook(
            "preamble = 1\n# +\nvalue = 2\n# -\n# + [markdown]\n# text\n# -\n",
            "light",
            "python",
        )
        .unwrap();
        assert_eq!(light.cells.len(), 3);
        let plain = script_to_notebook("value = 1\n", "light", "python").unwrap();
        assert_eq!(plain.cells.len(), 1);
        assert!(
            script_to_notebook("# %%not-a-marker\nvalue = 1\n", "percent", "python")
                .unwrap()
                .cells
                .iter()
                .any(|cell| matches!(cell, Cell::Code { .. }))
        );
        let marker_metadata =
            script_to_notebook("# %% {invalid-json}\nvalue = 1\n", "percent", "python").unwrap();
        assert_eq!(
            marker_metadata.cells[0].metadata().name.as_deref(),
            Some("{invalid-json}")
        );

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.md");
        let output = directory.path().join("nested/converted");
        fs::write(&source, "# Title\n\n```python\nprint(1)\n```\n").unwrap();
        let formats = [
            "md",
            "markdown",
            "notebook",
            "html",
            "py:light",
            "R:percent",
            "jl:light",
            "m:light",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        let results =
            transform_file(&source, &output, &formats, &TransformOptions::default()).unwrap();
        assert_eq!(results.len(), formats.len());
        assert!(output.with_extension("myst.md").is_file());
        assert!(output.with_extension("ipynb").is_file());
        assert!(output.with_extension("html").is_file());
        assert!(output.with_extension("R").is_file());
        assert!(output.with_extension("jl").is_file());
        assert!(output.with_extension("m").is_file());
        assert!(
            transform_file(
                &source,
                &output,
                &["unsupported".into()],
                &TransformOptions::default()
            )
            .is_err()
        );

        for (extension, source_text) in [
            ("py", "value = 1\n"),
            ("R", "value <- 1\n"),
            ("jl", "value = 1\n"),
            ("m", "value = 1\n"),
        ] {
            let script_source = directory.path().join(format!("input.{extension}"));
            fs::write(&script_source, source_text).unwrap();
            transform_file(
                &script_source,
                &output,
                &["myst".into()],
                &TransformOptions::default(),
            )
            .unwrap();
        }
        let percent_source = directory.path().join("input.py");
        fs::write(&percent_source, "# %%\nvalue = 1\n").unwrap();
        transform_file(
            &percent_source,
            &output,
            &["myst".into()],
            &TransformOptions::default(),
        )
        .unwrap();

        let dispatch = directory.path().join("dispatch.ipynb");
        fs::write(
            &dispatch,
            r##"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"cell_type":"markdown","metadata":{},"source":"text"}]}"##,
        )
        .unwrap();
        transform_file(&dispatch, &output, &[], &TransformOptions::default()).unwrap();
        fs::write(
            &dispatch,
            r##"{"nbformat":4,"nbformat_minor":0,"metadata":{},"cells":[]}"##,
        )
        .unwrap();
        transform_file(&dispatch, &output, &[], &TransformOptions::default()).unwrap();
        fs::write(
            &dispatch,
            r##"{"nbformat":3,"nbformat_minor":0,"metadata":{},"worksheets":[]}"##,
        )
        .unwrap();
        transform_file(&dispatch, &output, &[], &TransformOptions::default()).unwrap();
    }

    #[test]
    fn covers_workflow_config_and_manifest_fallbacks() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.yml");
        fs::write(&config_path, "not: a workflow\n").unwrap();
        assert!(load_workflow_config(&config_path).is_err());
        fs::write(&config_path, "[invalid\n").unwrap();
        assert!(load_workflow_config(&config_path).is_err());
        fs::write(
            &config_path,
            "output_formats: [myst]\nmanifest: ./manifest.json\n",
        )
        .unwrap();
        let config = load_workflow_config(&config_path).unwrap();
        assert_eq!(config.manifest, directory.path().join("manifest.json"));
        let absolute_manifest = directory.path().join("absolute.json");
        fs::write(
            &config_path,
            format!(
                "output_formats: [myst]\nmanifest: {}\n",
                absolute_manifest.display()
            ),
        )
        .unwrap();
        assert_eq!(
            load_workflow_config(&config_path).unwrap().manifest,
            absolute_manifest
        );

        let source_root = directory.path().join("source");
        let output_dir = directory.path().join("output");
        fs::create_dir_all(&source_root).unwrap();
        let source = source_root.join("note.md");
        fs::write(&source, "# Title\n").unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let manifest = TransformManifest {
            source_root: source_root.clone(),
            output_dir: output_dir.clone(),
            transform_fingerprint: "fingerprint".into(),
            files: BTreeMap::from([(
                "note.md".into(),
                ManifestFile {
                    source: "note.md".into(),
                    sha256: sha256_path(&source).unwrap(),
                    outputs: BTreeMap::new(),
                    ..ManifestFile::default()
                },
            )]),
            ..TransformManifest::default()
        };
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(
            transform_manifest(&manifest_path, true)
                .unwrap()
                .transformed,
            1
        );
        assert!(transform_manifest(&manifest_path, false).is_err());

        let successful_manifest = TransformManifest {
            output_formats: vec!["myst".into()],
            files: BTreeMap::from([(
                "note.md".into(),
                ManifestFile {
                    source: "note.md".into(),
                    sha256: sha256_path(&source).unwrap(),
                    outputs: BTreeMap::from([(
                        "myst".into(),
                        output_dir
                            .join("note.myst.md")
                            .to_string_lossy()
                            .into_owned(),
                    )]),
                    ..ManifestFile::default()
                },
            )]),
            ..manifest.clone()
        };
        fs::write(
            &manifest_path,
            serde_json::to_string(&successful_manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            transform_manifest(&manifest_path, false)
                .unwrap()
                .transformed,
            1
        );

        let error_manifest = TransformManifest {
            output_formats: vec!["unsupported".into()],
            files: BTreeMap::from([(
                "note.md".into(),
                ManifestFile {
                    source: "note.md".into(),
                    sha256: sha256_path(&source).unwrap(),
                    outputs: BTreeMap::from([("unsupported".into(), "ignored".into())]),
                    ..ManifestFile::default()
                },
            )]),
            ..manifest.clone()
        };
        fs::write(
            &manifest_path,
            serde_json::to_string(&error_manifest).unwrap(),
        )
        .unwrap();
        assert!(transform_manifest(&manifest_path, false).is_err());

        let default_manifest = TransformManifest {
            output_formats: Vec::new(),
            files: BTreeMap::new(),
            ..manifest
        };
        fs::write(
            &manifest_path,
            serde_json::to_string(&default_manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            transform_manifest(&manifest_path, false)
                .unwrap()
                .transformed,
            0
        );
        let parsed: TransformManifest = serde_json::from_str(
            &serde_json::json!({"source_root":"source","output_dir":"output","files":{}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(parsed.version, 1);
    }
}
