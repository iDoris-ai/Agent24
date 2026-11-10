//! Experimental ONNX encoder + logistic-regression head used by D1-1.
//! Model files are external; see `eval/decide/bench/scripts/export_onnx.py`.

use std::{fs, path::Path};

use ort::{session::Session, value::Tensor};
use serde::Deserialize;
use tokenizers::Tokenizer;

#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub label: String,
    pub p: f32,
}

#[derive(Deserialize)]
struct LinearHead {
    classes: Vec<String>,
    coefficients: Vec<Vec<f32>>,
    intercepts: Vec<f32>,
}

impl LinearHead {
    fn validate(&self) -> Result<(), String> {
        let binary = self.classes.len() == 2 && self.coefficients.len() == 1;
        if self.classes.len() < 2
            || (!binary && self.coefficients.len() != self.classes.len())
            || self.intercepts.len() != self.coefficients.len()
            || self
                .coefficients
                .windows(2)
                .any(|rows| rows[0].len() != rows[1].len())
        {
            return Err("classifier head dimensions do not match its classes".into());
        }
        Ok(())
    }

    fn validate_width(&self, width: usize) -> Result<(), String> {
        if self.coefficients.iter().any(|row| row.len() != width) {
            return Err("classifier head width does not match encoder output".into());
        }
        Ok(())
    }
}

fn validate_hidden_shape(dims: &[usize], tokens: usize) -> Result<(), String> {
    if dims.len() != 3 || dims[0] != 1 || dims[1] != tokens {
        return Err(format!("unexpected encoder output shape: {dims:?}"));
    }
    Ok(())
}

pub struct OnnxEmbeddingClassifier {
    session: Session,
    tokenizer: Tokenizer,
    head: LinearHead,
}

impl OnnxEmbeddingClassifier {
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");
        let head_path = model_dir.join("head_recall_gate.json");
        let session = Session::builder()
            .map_err(display_error)?
            .commit_from_file(model_path)
            .map_err(display_error)?;
        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(display_error)?;
        let head: LinearHead = serde_json::from_slice(&fs::read(head_path).map_err(display_error)?)
            .map_err(display_error)?;
        head.validate()?;
        Ok(Self {
            session,
            tokenizer,
            head,
        })
    }

    pub fn predict(&mut self, text: &str) -> Result<Prediction, String> {
        let encoding = self
            .tokenizer
            .encode(format!("query: {text}"), true)
            .map_err(display_error)?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|id| i64::from(*id)).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|id| i64::from(*id))
            .collect();
        let len = ids.len();
        let outputs = self.session.run(ort::inputs! {
            "input_ids" => Tensor::from_array(([1, len], ids)).map_err(display_error)?,
            "attention_mask" => Tensor::from_array(([1, len], mask.clone())).map_err(display_error)?,
        }).map_err(display_error)?;
        let hidden = outputs[0]
            .try_extract_array::<f32>()
            .map_err(display_error)?;
        let dims = hidden.shape();
        validate_hidden_shape(dims, len)?;
        let width = dims[2];
        self.head.validate_width(width)?;
        let mut embedding = vec![0.0_f32; width];
        let count = mask.iter().filter(|&&value| value != 0).count() as f32;
        for token in 0..len {
            if mask[token] != 0 {
                for column in 0..width {
                    embedding[column] += hidden[[0, token, column]] / count;
                }
            }
        }
        let norm = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if norm > 0.0 {
            embedding.iter_mut().for_each(|value| *value /= norm);
        }
        let logits: Vec<f32> = self
            .head
            .coefficients
            .iter()
            .zip(&self.head.intercepts)
            .map(|(weights, bias)| {
                weights
                    .iter()
                    .zip(&embedding)
                    .map(|(w, x)| w * x)
                    .sum::<f32>()
                    + bias
            })
            .collect();
        let binary_sklearn_head = self.head.classes.len() == 2 && self.head.coefficients.len() == 1;
        let (best, probability) = if binary_sklearn_head {
            let positive = 1.0 / (1.0 + (-logits[0]).exp());
            if positive >= 0.5 {
                (1, positive)
            } else {
                (0, 1.0 - positive)
            }
        } else {
            let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let exp: Vec<f32> = logits
                .iter()
                .map(|value| (value - max_logit).exp())
                .collect();
            let total = exp.iter().sum::<f32>();
            let best = exp
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i)
                .ok_or_else(|| "classifier head has no classes".to_string())?;
            (best, exp[best] / total)
        };
        Ok(Prediction {
            label: self.head.classes[best].clone(),
            p: probability,
        })
    }
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{LinearHead, validate_hidden_shape};

    #[test]
    fn invalid_head_and_output_dimensions_are_rejected() {
        let head = LinearHead {
            classes: vec!["a".into(), "b".into(), "c".into()],
            coefficients: vec![vec![0.1; 2]],
            intercepts: vec![0.0],
        };
        assert!(head.validate().is_err());
        assert!(validate_hidden_shape(&[1, 768], 1).is_err());
        let wrong_width = LinearHead {
            classes: vec!["a".into(), "b".into()],
            coefficients: vec![vec![0.1; 2]],
            intercepts: vec![0.0],
        };
        assert!(wrong_width.validate_width(3).is_err());
    }
}
