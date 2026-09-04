#![no_main]

use libfuzzer_sys::fuzz_target;
use nbconvertrs::{
    export_notebook_with_options, extract_resources, source_to_notebook, ExportOptions,
    TransformOptions,
};

const INPUT_FORMATS: &[&str] = &[
    "myst", "quarto", "pandoc", "py:percent", "py:light", "ipynb",
];
const OUTPUT_FORMATS: &[&str] = &[
    "myst", "html", "rst", "asciidoc", "quarto", "pandoc", "py:percent", "py:light",
    "ipynb",
];

fuzz_target!(|data: &[u8]| {
    let Some((&input_selector, rest)) = data.split_first() else {
        return;
    };
    let input_format = INPUT_FORMATS[input_selector as usize % INPUT_FORMATS.len()];
    let output_selector = rest.first().copied().unwrap_or(input_selector);
    let output_format = OUTPUT_FORMATS[output_selector as usize % OUTPUT_FORMATS.len()];
    let source = String::from_utf8_lossy(rest.get(1..).unwrap_or_default());
    let Ok(notebook) = source_to_notebook(&source, input_format, &TransformOptions::default()) else {
        return;
    };

    let _resources = extract_resources(&notebook);
    let result = export_notebook_with_options(
        &notebook,
        output_format,
        ExportOptions::default(),
    )
    .expect("every registered output format must export a valid notebook");
    assert!(!result.mime_type.is_empty());
    assert!(!result.output_extension.is_empty());
    if output_format == "ipynb" {
        source_to_notebook(&result.body, "ipynb", &TransformOptions::default())
            .expect("notebook export must remain valid notebook JSON");
    }
});
