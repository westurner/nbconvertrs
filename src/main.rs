use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::Parser;
use nbconvertrs::{
    SyncDirection, TransformOptions, detect_input_format, export_notebook, load_workflow_config,
    source_to_notebook, sync_pair, transform_file_with_input_format, transform_manifest,
    transform_manifest_with_config,
};

#[derive(Debug, Parser)]
#[command(
    name = "nbconvertrs",
    version,
    about = "Transform Markdown, scripts, and Jupyter notebooks",
    after_help = "EXAMPLES:\n  nbconvertrs README.md --to ipynb --output build/README\n  nbconvertrs notebook.ipynb --to html --stdout\n  cat notebook.py | nbconvertrs - --from py:percent --to ipynb --stdout\n  nbconvertrs --indir docs --outdir build/docs --out-format myst,ipynb\n  nbconvertrs note.ipynb --sync --output note.md --to myst\n  nbconvertrs --manifest .tmp/workflow/chat-manifest.json --dry-run\n\nFORMAT NAMES:\n  See README.md, 'Input and output formats', for all names, aliases, and extensions.\n  Input override:  --from py:percent\n  One output:     --to html\n  Multiple outputs: --out-format myst,ipynb"
)]
struct Args {
    /// Input file. Use `-` for stdin with --stdout, or use --indir for a directory workflow.
    source: Option<PathBuf>,
    /// Recursively convert Markdown and MyST files beneath this directory.
    #[arg(long, conflicts_with = "source")]
    indir: Option<PathBuf>,
    /// Output base path for a single source; the format extension is appended.
    #[arg(long, conflicts_with = "indir")]
    output: Option<PathBuf>,
    /// Destination directory for --indir (defaults to INDIR/converted).
    #[arg(long, visible_alias = "output-dir", conflicts_with = "source")]
    outdir: Option<PathBuf>,
    /// Write one rendered body to stdout instead of creating files.
    #[arg(long, conflicts_with_all = ["output", "outdir", "indir"])]
    stdout: bool,
    /// Comma-separated output formats; defaults to myst,ipynb.
    #[arg(
        long,
        value_delimiter = ',',
        conflicts_with = "to",
        value_name = "FORMAT[,FORMAT...]"
    )]
    out_format: Option<Vec<String>>,
    /// One output format, such as `myst`, `ipynb`, `html`, or `py:percent`.
    #[arg(long, conflicts_with = "out_format")]
    to: Option<String>,
    /// Override input format detection, for example `py:percent`.
    #[arg(long)]
    from: Option<String>,
    /// Synchronize a notebook source with the text path supplied by --output.
    #[arg(long)]
    sync: bool,
    /// Execute notebook cells before conversion (not available yet).
    #[arg(long)]
    execute: bool,
    /// Markdown cell-splitting policy, currently `m1`.
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
    if args.execute {
        return Err(
            "--execute is not available yet; use a Jupyter kernel adapter or static conversion"
                .into(),
        );
    }
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
    if args.stdout {
        if args.manifest.is_some() || args.config.is_some() {
            return Err("--stdout cannot be combined with workflow mode".into());
        }
        let source = args.source.ok_or("--stdout requires a source file or -")?;
        let format = match (&args.to, &args.out_format) {
            (Some(format), None) => format.clone(),
            (None, Some(formats)) if formats.len() == 1 => formats[0].clone(),
            (None, None) => return Err("--stdout requires --to FORMAT".into()),
            _ => return Err("--stdout requires exactly one output format".into()),
        };
        let mut source_text = String::new();
        if source == Path::new("-") {
            io::stdin().read_to_string(&mut source_text)?;
        } else {
            source_text = fs::read_to_string(&source)?;
        }
        let input_format = args
            .from
            .clone()
            .unwrap_or_else(|| detect_input_format(&source, &source_text).canonical_name());
        let notebook = source_to_notebook(
            &source_text,
            &input_format,
            &TransformOptions {
                cell_split: args.transform_cell_split.clone(),
            },
        )?;
        let result = export_notebook(&notebook, &format)?;
        print!("{}", result.body);
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
