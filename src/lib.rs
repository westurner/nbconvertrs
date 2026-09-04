//! Jupytext-compatible Markdown transforms used by the sustainablefactory
//! workflow and optionally by DocIndex.

use std::fs;
use std::path::{Path, PathBuf};

use nbformat::Notebook;
use nbformat::v4::{Cell, CellId, CellMetadata, Notebook as NotebookV4};

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("notebook error: {0}")]
    Notebook(#[from] nbformat::NotebookError),
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
    let mut line_index = 0;

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

    while line_index < lines.len() {
        let line = lines[line_index];
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
                .or_else(|| info.strip_prefix("{code-cell "))
                .or_else(|| info.strip_prefix('{'))
                .unwrap_or(info)
                .split_whitespace()
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("python");
            let mut metadata = CellMetadata::default();
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
        metadata: Default::default(),
        nbformat: 4,
        nbformat_minor: 5,
        cells,
    })
}

/// Render an nbformat v4 notebook in Jupytext Markdown form.
pub fn notebook_to_markdown(notebook: &NotebookV4) -> String {
    let mut output = String::new();
    for (index, cell) in notebook.cells.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            output.push('\n');
        }
        match cell {
            Cell::Markdown { source, .. } => output.push_str(&source.concat()),
            Cell::Raw { source, .. } => {
                output.push_str("```{raw-cell}\n");
                output.push_str(&source.concat());
                output.push_str("```\n");
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
    let notebook = markdown_to_notebook(&fs::read_to_string(source)?, options)?;
    let output_base = output_base.as_ref();
    if let Some(parent) = output_base.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut outputs = Vec::new();
    for format in formats {
        let (suffix, contents) = match format.as_str() {
            "myst" | "md" | "markdown" => ("myst.md", notebook_to_markdown(&notebook)),
            "ipynb" | "notebook" => ("ipynb", notebook_to_json(&notebook)?),
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

fn cell_id(index: usize) -> CellId {
    CellId::try_from(format!("cell-{index}"))
        .expect("generated notebook cell IDs are valid by construction")
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
}
