#![no_main]

use libfuzzer_sys::fuzz_target;
use nbconvertrs::{notebook_to_json, source_to_notebook, TransformOptions};

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let Ok(notebook) = source_to_notebook(&source, "ipynb", &TransformOptions::default()) else {
        return;
    };

    let json = notebook_to_json(&notebook).expect("valid notebook input must serialize");
    let reparsed = source_to_notebook(&json, "ipynb", &TransformOptions::default())
        .expect("serialized notebook JSON must parse");
    let normalized = notebook_to_json(&reparsed).expect("reparsed notebook must serialize");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).expect("serialized JSON is valid"),
        serde_json::from_str::<serde_json::Value>(&normalized).expect("normalized JSON is valid")
    );
});
