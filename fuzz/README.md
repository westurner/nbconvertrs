# nbconvertrs fuzz targets

This package exercises the public `nbconvertrs` conversion boundary with
libFuzzer. It is intentionally separate from the production Cargo workspace,
does not execute notebook code, and does not require external converters or
network access.

## Targets

| Target | Boundary exercised |
| --- | --- |
| `markdown_to_notebook` | Markdown/MyST parsing and notebook JSON round trips |
| `script_to_notebook` | Python percent/light scripts and notebook JSON round trips |
| `notebook_json` | Arbitrary notebook JSON input and normalized serialization |
| `export_dispatch` | Input format dispatch, exporters, and resource extraction |

## Local runs

Install `cargo-fuzz` and a nightly Rust toolchain, then run these commands in
this directory:

```text
cargo +nightly fuzz build
cargo +nightly fuzz run markdown_to_notebook -- -max_total_time=10
cargo +nightly fuzz run script_to_notebook -- -max_total_time=10
cargo +nightly fuzz run notebook_json -- -max_total_time=10
cargo +nightly fuzz run export_dispatch -- -max_total_time=10
```

Each target automatically reads its checked-in seeds from
`corpus/<target>/`. Minimized crashers are written to `artifacts/<target>/`;
that directory is ignored so only a reproduced regression input should be
committed.

## Full run

`full_fuzz.sh` is the checked-in full-run profile. It builds the fuzz package,
runs every target against its seed corpus, bounds individual inputs at 1 MiB,
and gives each target five minutes by default:

```text
./full_fuzz.sh > /tmp/nbconvertrs-fuzz.log 2>&1
tail -f /tmp/nbconvertrs-fuzz.log
```

Override the per-target duration or input bound with environment variables:

```text
FUZZ_MAX_TOTAL_TIME=1800 FUZZ_MAX_LEN=2097152 ./full_fuzz.sh \
	> /tmp/nbconvertrs-fuzz.log 2>&1
```

The profile runs `markdown_to_notebook`, `script_to_notebook`, `notebook_json`,
and `export_dispatch` in that order. A target failure stops the profile and
leaves its output in the log for diagnosis.