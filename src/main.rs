use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use nbconvertrs::{TransformOptions, transform_file};

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
    #[arg(long, value_delimiter = ',', default_value = "myst,ipynb")]
    out_format: Vec<String>,
    #[arg(long)]
    transform_cell_split: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let options = TransformOptions {
        cell_split: args.transform_cell_split,
    };
    let formats = args.out_format;
    match (args.source, args.indir) {
        (Some(source), None) => {
            let output = args
                .output
                .unwrap_or_else(|| PathBuf::from(source.file_stem().unwrap_or_default()));
            for result in transform_file(&source, output, &formats, &options)? {
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
                for result in transform_file(&source, output, &formats, &options)? {
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
