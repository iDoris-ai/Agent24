use std::{env, fs, path::PathBuf, time::Instant};

use agent24_decide::onnx::OnnxEmbeddingClassifier;
use serde::Deserialize;
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

#[derive(Deserialize)]
struct Reference {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    input: String,
    label: String,
    p: f32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_dir = PathBuf::from(env::var("agent24_onnx_spike_dir")?);
    let reference: Reference =
        serde_json::from_slice(&fs::read(model_dir.join("recall_gate_python.json"))?)?;
    let mut model = OnnxEmbeddingClassifier::load(&model_dir).map_err(std::io::Error::other)?;
    let pid = get_current_pid()?;
    let mut system = System::new();
    let mut peak_rss = 0;
    let mut latencies = Vec::with_capacity(reference.items.len());
    let mut max_probability_error = 0.0_f32;
    let mut label_disagreements = Vec::new();
    let mut probability_outliers = Vec::new();
    for item in reference.items {
        let started = Instant::now();
        let prediction = model.predict(&item.input).map_err(std::io::Error::other)?;
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        max_probability_error = max_probability_error.max((prediction.p - item.p).abs());
        if prediction.label != item.label {
            label_disagreements.push(item.id.clone());
        }
        if (prediction.p - item.p).abs() > 0.02 {
            probability_outliers.push(item.id);
        }
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        if let Some(process) = system.process(pid) {
            peak_rss = peak_rss.max(process.memory());
        }
    }
    latencies.sort_by(f64::total_cmp);
    let percentile =
        |fraction: f64| latencies[((latencies.len() - 1) as f64 * fraction).ceil() as usize];
    println!(
        "{}",
        serde_json::json!({
            "items": latencies.len(),
            "max_probability_error": max_probability_error,
            "label_disagreements_near_0_5_boundary": label_disagreements,
            "probability_outliers_over_0_02": probability_outliers,
            "p50_latency_ms": percentile(0.50),
            "p95_latency_ms": percentile(0.95),
            "peak_process_rss_mb_sampled": peak_rss as f64 / (1024.0 * 1024.0),
        })
    );
    Ok(())
}
