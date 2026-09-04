#![no_main]

use libfuzzer_sys::fuzz_target;
use nbconvertrs::{notebook_to_json, script_to_notebook, source_to_notebook, TransformOptions};

fuzz_target!(|data: &[u8]| {
    let (format, source_bytes) = match data.split_first() {
        Some((selector, rest)) if selector & 1 == 0 => ("percent", rest),
        Some((_, rest)) => ("light", rest),
        None => ("percent", data),
    };
    let source = String::from_utf8_lossy(source_bytes);
    let Ok(notebook) = script_to_notebook(&source, format, "python") else {
        return;
    };

    let json = notebook_to_json(&notebook).expect("a parsed script notebook must serialize");
    let reparsed = source_to_notebook(&json, "ipynb", &TransformOptions::default())
        .expect("serialized script notebook JSON must parse");
    assert_eq!(
        notebook_to_json(&reparsed).expect("a reparsed script notebook must serialize"),
        json
    );
});
