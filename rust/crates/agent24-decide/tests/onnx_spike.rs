use std::{env, fs, path::PathBuf};

use agent24_decide::onnx::{OnnxEmbeddingClassifier, Prediction};
use serde::Deserialize;

#[derive(Deserialize)]
struct PythonReference {
    items: Vec<ReferenceItem>,
}

#[derive(Deserialize)]
struct ReferenceItem {
    id: String,
    input: String,
    label: String,
    p: f32,
    input_ids: Vec<u32>,
}

#[test]
fn e5_base_recall_gate_matches_python_int8_reference_when_configured()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(model_dir) = env::var("agent24_onnx_spike_dir") else {
        eprintln!("skipping ONNX spike; set agent24_onnx_spike_dir to exported e5-base assets");
        return Ok(());
    };
    let model_dir = PathBuf::from(model_dir);
    let reference: PythonReference =
        serde_json::from_slice(&fs::read(model_dir.join("recall_gate_python.json"))?)?;
    let mut model = OnnxEmbeddingClassifier::load(&model_dir).map_err(std::io::Error::other)?;
    let tokenizer = tokenizers::Tokenizer::from_file(model_dir.join("tokenizer.json"))
        .map_err(std::io::Error::other)?;

    assert_eq!(
        reference.items.len(),
        221,
        "full recall_gate subset is compared"
    );
    let sample_count = reference.items.len();
    let mut label_mismatches = Vec::new();
    let mut max_probability_error = 0.0_f32;
    for row in reference.items {
        let tokenized = tokenizer
            .encode(format!("query: {}", row.input), true)
            .map_err(std::io::Error::other)?;
        assert_eq!(
            tokenized.get_ids(),
            row.input_ids,
            "tokenizer mismatch for {}",
            row.id
        );
        let actual: Prediction = model.predict(&row.input).map_err(std::io::Error::other)?;
        let probability_error = (actual.p - row.p).abs();
        max_probability_error = max_probability_error.max(probability_error);
        if actual.label != row.label {
            label_mismatches.push((row.id, row.label, row.p, actual.label, actual.p));
        }
    }
    assert!(
        label_mismatches.iter().all(|(_, _, python_p, _, rust_p)| {
            (python_p - 0.5).abs() <= 0.02 && (rust_p - 0.5).abs() <= 0.02
        }),
        "labels may disagree only within the ±0.02 decision-boundary band: {label_mismatches:?}"
    );
    assert!(
        label_mismatches.len() <= sample_count / 100,
        "label agreement must be at least 99%; disagreements: {label_mismatches:?}"
    );
    assert!(
        max_probability_error <= 0.02,
        "max probability error {max_probability_error} > 0.02"
    );
    Ok(())
}
