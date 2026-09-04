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