//! Jupytext-compatible Markdown transforms used by the sustainablefactory
//! workflow and optionally by DocIndex.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nbformat::Notebook;
use nbformat::v4::{Cell, CellId, CellMetadata, Notebook as NotebookV4};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("notebook error: {0}")]
    Notebook(#[from] nbformat::NotebookError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid transform configuration: {0}")]
    Configuration(String),
    #[error("unsupported output format {0:?}")]
    UnsupportedFormat(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransformOptions {
    /// Split Markdown cells before level-one headings when set to `m1`.
    pub cell_split: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformOutput {
    pub path: PathBuf,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManifestFile {
    pub source: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    #[serde(default)]
    pub transformed_sha256: Option<String>,
    #[serde(default)]
    pub transformed_fingerprint: Option<String>,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransformManifest {
    #[serde(default = "manifest_version")]
    pub version: u32,
    pub source_root: PathBuf,
    pub output_dir: PathBuf,
    #[serde(default)]
    pub temp_dir: Option<PathBuf>,
    #[serde(default)]
    pub output_formats: Vec<String>,
    #[serde(default)]
    pub transform_fingerprint: String,
    #[serde(default)]
    pub files: BTreeMap<String, ManifestFile>,
    #[serde(default)]
    pub last_transform: Option<TransformSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransformSummary {
    pub transformed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowConfig {
    pub manifest: PathBuf,
    pub output_formats: Vec<String>,
    pub cell_split: Option<String>,
}

fn manifest_version() -> u32 {
    1
}

/// Convert a Jupytext-style Markdown document into an nbformat v4 notebook.
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
            let source = source;
            let metadata = metadata.unwrap_or_default();
            let id = cell_id(index);
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
                    unreachable!("HTML regions only describe Markdown or raw cells")
                }
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

/// Render an nbformat v4 notebook in Jupytext Markdown form.
pub fn notebook_to_markdown(notebook: &NotebookV4) -> String {
    let mut output = String::new();
    if let Ok(metadata) = serde_yaml::to_string(&notebook.metadata) {
        if metadata.trim() != "{}" {
            output.push_str("---\n");
            output.push_str(&metadata);
            output.push_str("---\n\n");
        }
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
    if let Ok(metadata) = serde_yaml::to_string(&notebook.metadata) {
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

/// Serialize a notebook using runtimed's nbformat-compatible writer.
pub fn notebook_to_json(notebook: &NotebookV4) -> Result<String, TransformError> {
    Ok(nbformat::serialize_notebook(&Notebook::V4(
        notebook.clone(),
    ))?)
}

/// Convert Markdown to the requested output files using one in-process API.
pub fn transform_file(
    source: impl AsRef<Path>,
    output_base: impl AsRef<Path>,
    formats: &[String],
    options: &TransformOptions,
) -> Result<Vec<TransformOutput>, TransformError> {
    let source = source.as_ref();
    let source_text = fs::read_to_string(source)?;
    let notebook = match source.extension().and_then(|value| value.to_str()) {
        Some("ipynb") => match nbformat::parse_notebook(&source_text)? {
            Notebook::V4(notebook) => notebook,
            Notebook::V4QuirksMode(notebook) => notebook.repair(),
            Notebook::Legacy(notebook) => nbformat::upgrade_legacy_notebook(notebook)
                .map_err(|error| TransformError::Configuration(error.to_string()))?,
            Notebook::V3(notebook) => nbformat::upgrade_v3_notebook(notebook)
                .map_err(|error| TransformError::Configuration(error.to_string()))?,
            _ => {
                return Err(TransformError::Configuration(
                    "unsupported notebook variant".into(),
                ));
            }
        },
        Some("py" | "R" | "jl" | "m") => {
            let language = match source.extension().and_then(|value| value.to_str()) {
                Some("R") => "r",
                Some("jl") => "julia",
                Some("m") => "matlab",
                _ => "python",
            };
            let comment_prefix = script_comment_prefix(language);
            let format = if source_text
                .lines()
                .any(|line| script_percent_marker(line, comment_prefix).is_some())
            {
                "percent"
            } else {
                "light"
            };
            script_to_notebook(&source_text, format, language)?
        }
        _ => markdown_to_notebook(&source_text, options)?,
    };
    let output_base = output_base.as_ref();
    if let Some(parent) = output_base.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut outputs = Vec::new();
    for format in formats {
        let (suffix, contents) = match format.as_str() {
            "myst" | "md" | "markdown" => ("myst.md", notebook_to_markdown(&notebook)),
            "ipynb" | "notebook" => ("ipynb", notebook_to_json(&notebook)?),
            value if value.ends_with(":percent") => {
                let extension = value.split(':').next().unwrap_or("py");
                (
                    extension,
                    notebook_to_script(&notebook, "percent", extension),
                )
            }
            value if value.ends_with(":light") => {
                let extension = value.split(':').next().unwrap_or("py");
                (extension, notebook_to_script(&notebook, "light", extension))
            }
            other => return Err(TransformError::UnsupportedFormat(other.into())),
        };
        let path = output_base.with_extension(suffix);
        fs::write(&path, contents)?;
        outputs.push(TransformOutput {
            path,
            format: format.clone(),
        });
    }
    Ok(outputs)
}

/// Load the workflow settings from the sustainablefactory `_toc.yml` shape.
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
    Start((CellTypeTag, CellMetadata)),
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
    Some(ScriptMarker::Start(parse_script_options(rest)))
}

fn script_light_marker(line: &str, comment_prefix: &str) -> Option<ScriptMarker> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix(comment_prefix)?.trim();
    if rest == "-" {
        return Some(ScriptMarker::End);
    }
    let options = rest.strip_prefix('+')?;
    Some(ScriptMarker::Start(parse_script_options(options)))
}

fn script_comment_prefix(language: &str) -> &'static str {
    match language.to_ascii_lowercase().as_str() {
        "matlab" | "m" => "%",
        _ => "#",
    }
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
}
