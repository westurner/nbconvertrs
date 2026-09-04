#![no_main]

use libfuzzer_sys::fuzz_target;
use nbconvertrs::{markdown_to_notebook, notebook_to_json, source_to_notebook, TransformOptions};

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let options = TransformOptions::default();
    let Ok(notebook) = markdown_to_notebook(&source, &options) else {
        return;
    };

    let json = notebook_to_json(&notebook).expect("a parsed notebook must serialize");
    let reparsed = source_to_notebook(&json, "ipynb", &options)
        .expect("serialized notebook JSON must parse at the notebook boundary");
    let normalized = notebook_to_json(&reparsed).expect("a reparsed notebook must serialize");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).expect("exported JSON is valid"),
        serde_json::from_str::<serde_json::Value>(&normalized).expect("normalized JSON is valid")
    );
});
