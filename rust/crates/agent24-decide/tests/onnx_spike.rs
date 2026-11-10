use std::{env, fs, path::PathBuf, time::Instant};

use agent24_decide::onnx::OnnxEmbeddingClassifier;
use serde::Deserialize;
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

#[derive(Deserialize)]
struct PythonReference {
    items: Vec<ReferenceItem>,
}

#[derive(Deserialize)]
struct ReferenceItem {
    input: String,
    label: String,
    p: f32,
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
    assert_eq!(
        reference.items.len(),
        221,
        "full recall_gate subset is compared"
    );
    let sample_count = reference.items.len();
    let pid = get_current_pid()?;
    let mut system = System::new();
    let mut peak_rss = 0;
    let mut latencies = Vec::with_capacity(sample_count);
    let mut label_mismatches = 0;
    let mut max_probability_error = 0.0_f32;
    for row in reference.items {
        let started = Instant::now();
        let actual = model.predict(&row.input).map_err(std::io::Error::other)?;
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        let probability_error = (actual.p - row.p).abs();
        max_probability_error = max_probability_error.max(probability_error);
        if actual.label != row.label {
            label_mismatches += 1;
            assert!((row.p - 0.5).abs() <= 0.02 && (actual.p - 0.5).abs() <= 0.02);
        }
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        if let Some(process) = system.process(pid) {
            peak_rss = peak_rss.max(process.memory());
        }
    }
    assert!(
        label_mismatches <= 2,
        "label agreement must be at least 99%; disagreements: {label_mismatches}"
    );
    assert!(
        max_probability_error <= 0.02,
        "max probability error {max_probability_error} > 0.02"
    );
    latencies.sort_by(f64::total_cmp);
    let percentile =
        |fraction: f64| latencies[((sample_count - 1) as f64 * fraction).ceil() as usize];
    println!(
        "items={sample_count} labels_disagree={label_mismatches} max_probability_error={max_probability_error} p50_ms={} p95_ms={} peak_rss_mb={}",
        percentile(0.50),
        percentile(0.95),
        peak_rss as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}
