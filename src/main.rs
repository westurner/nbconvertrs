use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use nbconvertrs::{
    SyncDirection, TransformOptions, load_workflow_config, sync_pair,
    transform_file_with_input_format, transform_manifest, transform_manifest_with_config,
};

#[derive(Debug, Parser)]
#[command(
    name = "nbconvertrs",
    version,
    about = "Transform Markdown and notebooks"
)]
struct Args {
    /// One Markdown source file. Use --indir for a directory workflow.
    source: Option<PathBuf>,
    #[arg(long, conflicts_with = "source")]
    indir: Option<PathBuf>,
    #[arg(long, conflicts_with = "indir")]
    output: Option<PathBuf>,
    #[arg(long, conflicts_with = "source")]
    outdir: Option<PathBuf>,
    #[arg(long, value_delimiter = ',', conflicts_with = "to")]
    out_format: Option<Vec<String>>,
    /// One output format, using aliases such as `myst`, `ipynb`, or `py:percent`.
    #[arg(long, conflicts_with = "out_format")]
    to: Option<String>,
    /// Override input format detection, for example `py:percent`.
    #[arg(long)]
    from: Option<String>,
    /// Synchronize a notebook source with the text path supplied by --output.
    #[arg(long)]
    sync: bool,
    #[arg(long)]
    transform_cell_split: Option<String>,
    /// Workflow `_toc.yml` used to locate the default manifest and transform settings.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Existing incremental workflow manifest.
    #[arg(long)]
    manifest: Option<PathBuf>,
    /// Report stale records without writing outputs or updating the manifest.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.sync {
        if args.manifest.is_some() || args.config.is_some() || args.indir.is_some() {
            return Err("--sync cannot be combined with workflow mode".into());
        }
        let notebook = args.source.ok_or("--sync requires a notebook source")?;
        let text = args
            .output
            .ok_or("--sync requires --output for the text pair")?;
        let format = match (args.to, args.out_format) {
            (Some(format), None) => format,
            (None, Some(formats)) if formats.len() == 1 => formats[0].clone(),
            (None, None) => return Err("--sync requires --to FORMAT".into()),
            _ => return Err("--sync requires exactly one text output format".into()),
        };
        let result = sync_pair(notebook, text, &format, &TransformOptions::default())?;
        let action = match result.direction {
            SyncDirection::NotebookToText => "notebook to text",
            SyncDirection::TextToNotebook => "text to notebook",
            SyncDirection::Unchanged => "unchanged pair",
        };
        println!(
            "synchronized {action}: {} and {}",
            result.notebook.display(),
            result.text.display()
        );
        return Ok(());
    }
    if args.manifest.is_some()
        || (args.source.is_none() && args.indir.is_none() && args.config.is_some())
    {
        let config = args.config.as_ref().map(load_workflow_config).transpose()?;
        let manifest = args
            .manifest
            .or_else(|| config.as_ref().map(|value| value.manifest.clone()))
            .ok_or("provide --manifest or --config for workflow mode")?;
        let summary = match config.as_ref() {
            Some(config) => transform_manifest_with_config(&manifest, args.dry_run, config)?,
            None => transform_manifest(&manifest, args.dry_run)?,
        };
        if args.dry_run {
            println!(
                "workflow transform: {} changed, {} skipped",
                summary.transformed, summary.skipped
            );
        } else {
            println!(
                "workflow transform: {} transformed, {} skipped",
                summary.transformed, summary.skipped
            );
        }
        return Ok(());
    }
    if args.dry_run || args.config.is_some() {
        return Err("--dry-run and --config require workflow mode".into());
    }
    if args.from.is_some() && args.source.is_none() {
        return Err("--from requires a source file".into());
    }
    let options = TransformOptions {
        cell_split: args.transform_cell_split,
    };
    let formats = args
        .to
        .map(|format| vec![format])
        .or(args.out_format)
        .unwrap_or_else(|| vec!["myst".into(), "ipynb".into()]);
    let input_format = args.from.as_deref();
    match (args.source, args.indir) {
        (Some(source), None) => {
            let output = args
                .output
                .unwrap_or_else(|| PathBuf::from(source.file_stem().unwrap_or_default()));
            for result in
                transform_file_with_input_format(&source, output, &formats, &options, input_format)?
            {
                println!("wrote {}", result.path.display());
            }
        }
        (None, Some(indir)) => {
            let outdir = args.outdir.unwrap_or_else(|| indir.join("converted"));
            for source in markdown_files(&indir)? {
                let relative = source.strip_prefix(&indir)?;
                let output = outdir.join(relative).with_extension(
                    relative
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .unwrap_or("document"),
                );
                for result in transform_file_with_input_format(
                    &source,
                    output,
                    &formats,
                    &options,
                    input_format,
                )? {
                    println!("wrote {}", result.path.display());
                }
            }
        }
        (None, None) => return Err("provide a source file or --indir".into()),
        (Some(_), Some(_)) => unreachable!("clap prevents conflicting source selectors"),
    }
    Ok(())
}

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(markdown_files(&path)?);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("md" | "markdown")
        ) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
