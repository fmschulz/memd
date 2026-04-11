use super::onnx_runtime;
use ndarray::{Array2, ArrayViewD, Axis};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use parking_lot::Mutex;
use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex as StdMutex, OnceLock};
use tokenizers::{EncodeInput, PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

const MODEL_URL: &str =
    "https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main/tokenizer.json";
const MODEL_FILENAME: &str = "ms-marco-minilm-l6-v2.onnx";
const TOKENIZER_FILENAME: &str = "ms-marco-minilm-l6-v2-tokenizer.json";
const MODEL_MIN_BYTES: u64 = 1_000_000;
const TOKENIZER_MIN_BYTES: u64 = 100_000;
const DEFAULT_MAX_LENGTH: usize = 256;

static SCORER: OnceLock<Result<OnnxCrossEncoderScorer, String>> = OnceLock::new();
static PANIC_HOOK_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

pub(crate) fn is_available() -> bool {
    catch_onnx_panic(|| get_scorer().is_ok()).unwrap_or(false)
}

pub(crate) fn score_pairs(query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
    catch_onnx_panic(|| {
        let scorer = get_scorer()?;
        scorer.score_pairs(query, docs)
    })?
}

fn get_scorer() -> Result<&'static OnnxCrossEncoderScorer, String> {
    SCORER
        .get_or_init(OnnxCrossEncoderScorer::new)
        .as_ref()
        .map_err(|err| err.clone())
}

fn catch_onnx_panic<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> T,
{
    let lock = PANIC_HOOK_LOCK.get_or_init(|| StdMutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "cross-encoder panic hook mutex poisoned".to_string())?;
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(old_hook);
    result.map_err(|_| "cross-encoder scorer panicked during ONNX initialization".to_string())
}

struct OnnxCrossEncoderScorer {
    session: Mutex<Session>,
    tokenizer: Mutex<Tokenizer>,
    has_token_type_ids: bool,
    max_length: usize,
}

impl OnnxCrossEncoderScorer {
    fn new() -> Result<Self, String> {
        if std::env::var("MEMD_CROSS_ENCODER_DISABLE").ok().as_deref() == Some("1") {
            return Err(
                "cross-encoder scorer disabled via MEMD_CROSS_ENCODER_DISABLE=1".to_string(),
            );
        }

        let cache_root = cache_dir()?;
        let ort_dylib_path = onnx_runtime::ensure_dylib(&cache_root)?;
        let model_path = resolve_model_path(&cache_root)?;
        let tokenizer_path = resolve_tokenizer_path(&cache_root)?;
        let session = Session::builder()
            .map_err(|err| format!("create ONNX session builder: {err}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|err| format!("set ONNX optimization level: {err}"))?
            .with_intra_threads(2)
            .map_err(|err| format!("set ONNX intra threads: {err}"))?
            .commit_from_file(&model_path)
            .map_err(|err| format!("load ONNX model '{}': {err}", model_path.display()))?;
        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|err| format!("load tokenizer '{}': {err}", tokenizer_path.display()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: DEFAULT_MAX_LENGTH,
                ..Default::default()
            }))
            .map_err(|err| format!("configure tokenizer truncation: {err}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));

        tracing::info!(
            model = %model_path.display(),
            tokenizer = %tokenizer_path.display(),
            onnxruntime = %ort_dylib_path.display(),
            has_token_type_ids,
            "initialized ONNX cross-encoder scorer"
        );

        Ok(Self {
            session: Mutex::new(session),
            tokenizer: Mutex::new(tokenizer),
            has_token_type_ids,
            max_length: DEFAULT_MAX_LENGTH,
        })
    }

    fn score_pairs(&self, query: &str, docs: &[String]) -> Result<Vec<f32>, String> {
        if query.trim().is_empty() || docs.is_empty() {
            return Ok(vec![0.0; docs.len()]);
        }

        let inputs: Vec<EncodeInput> = docs
            .iter()
            .map(|doc| EncodeInput::Dual(query.into(), doc.as_str().into()))
            .collect();
        let encodings = self
            .tokenizer
            .lock()
            .encode_batch(inputs, true)
            .map_err(|err| format!("tokenize query/document pairs: {err}"))?;
        let (input_ids, attention_mask, token_type_ids) = self.to_tensors(&encodings)?;
        self.run_model(input_ids, attention_mask, token_type_ids)
    }

    fn to_tensors(
        &self,
        encodings: &[tokenizers::Encoding],
    ) -> Result<(Array2<i64>, Array2<i64>, Array2<i64>), String> {
        let max_len = encodings
            .iter()
            .map(|enc| enc.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);
        if max_len == 0 {
            return Err("tokenizer produced zero-length input".to_string());
        }

        let batch = encodings.len();
        let mut input_ids = Array2::<i64>::zeros((batch, max_len));
        let mut attention_mask = Array2::<i64>::zeros((batch, max_len));
        let mut token_type_ids = Array2::<i64>::zeros((batch, max_len));
        for (i, enc) in encodings.iter().enumerate() {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let types = enc.get_type_ids();
            for j in 0..max_len {
                input_ids[[i, j]] = ids.get(j).copied().unwrap_or(0) as i64;
                attention_mask[[i, j]] = mask.get(j).copied().unwrap_or(0) as i64;
                token_type_ids[[i, j]] = types.get(j).copied().unwrap_or(0) as i64;
            }
        }

        Ok((input_ids, attention_mask, token_type_ids))
    }

    fn run_model(
        &self,
        input_ids: Array2<i64>,
        attention_mask: Array2<i64>,
        token_type_ids: Array2<i64>,
    ) -> Result<Vec<f32>, String> {
        let input_ids_tensor = TensorRef::from_array_view(input_ids.view())
            .map_err(|err| format!("create input_ids tensor: {err}"))?;
        let attention_mask_tensor = TensorRef::from_array_view(attention_mask.view())
            .map_err(|err| format!("create attention_mask tensor: {err}"))?;

        let mut session = self.session.lock();
        let outputs = if self.has_token_type_ids {
            let token_type_ids_tensor = TensorRef::from_array_view(token_type_ids.view())
                .map_err(|err| format!("create token_type_ids tensor: {err}"))?;
            session.run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
        } else {
            session.run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            ])
        }
        .map_err(|err| format!("cross-encoder inference failed: {err}"))?;

        let logits = if let Some(output) = outputs.get("logits") {
            output
                .try_extract_array::<f32>()
                .map_err(|err| format!("extract logits output: {err}"))?
                .into_owned()
        } else {
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(|err| format!("extract default output tensor: {err}"))?
                .into_owned()
        };
        Ok(logits_to_scores(logits.view()))
    }
}

fn logits_to_scores(logits: ArrayViewD<'_, f32>) -> Vec<f32> {
    if logits.ndim() == 1 {
        return logits.iter().copied().map(sigmoid).collect();
    }
    if logits.ndim() >= 2 {
        return (0..logits.shape()[0])
            .map(|row| {
                let value = logits
                    .index_axis(Axis(0), row)
                    .iter()
                    .copied()
                    .next()
                    .unwrap_or(0.0);
                sigmoid(value)
            })
            .collect();
    }
    vec![0.0]
}

fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

fn resolve_model_path(cache_root: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("MEMD_CROSS_ENCODER_MODEL_PATH") {
        return verify_existing_file(Path::new(&path), MODEL_MIN_BYTES, "model");
    }
    let cache_path = cache_root.join(MODEL_FILENAME);
    ensure_downloaded(&cache_path, MODEL_URL, MODEL_MIN_BYTES, "model")
}

fn resolve_tokenizer_path(cache_root: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("MEMD_CROSS_ENCODER_TOKENIZER_PATH") {
        return verify_existing_file(Path::new(&path), TOKENIZER_MIN_BYTES, "tokenizer");
    }
    let cache_path = cache_root.join(TOKENIZER_FILENAME);
    ensure_downloaded(&cache_path, TOKENIZER_URL, TOKENIZER_MIN_BYTES, "tokenizer")
}

fn cache_dir() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("MEMD_CROSS_ENCODER_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    let base = dirs::cache_dir().ok_or_else(|| "resolve cache directory".to_string())?;
    Ok(base.join("memd").join("cross-encoder"))
}

fn ensure_downloaded(
    path: &Path,
    url: &str,
    min_bytes: u64,
    label: &str,
) -> Result<PathBuf, String> {
    if let Ok(existing) = verify_existing_file(path, min_bytes, label) {
        return Ok(existing);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create {} cache dir '{}': {err}", label, parent.display()))?;
    }
    tracing::info!(label, url, destination = %path.display(), "downloading cross-encoder asset");
    let mut response = ureq::get(url)
        .call()
        .map_err(|err| format!("download {} from '{}': {err}", label, url))?
        .into_reader();
    let mut file =
        fs::File::create(path).map_err(|err| format!("create '{}': {err}", path.display()))?;
    io::copy(&mut response, &mut file)
        .map_err(|err| format!("write '{}': {err}", path.display()))?;
    verify_existing_file(path, min_bytes, label)
}

fn verify_existing_file(path: &Path, min_bytes: u64, label: &str) -> Result<PathBuf, String> {
    let metadata = fs::metadata(path).map_err(|err| format!("stat '{}': {err}", path.display()))?;
    if metadata.len() < min_bytes {
        return Err(format!(
            "{} file '{}' too small ({} bytes, expected at least {})",
            label,
            path.display(),
            metadata.len(),
            min_bytes
        ));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::score_pairs;

    #[test]
    #[ignore = "requires real ONNX runtime/model initialization and may download assets"]
    fn smoke_real_onnx_scores_relevant_pair_higher() {
        let docs = vec![
            "const validateEmailFormat = (str) => { return /^\\w+([.-]?\\w+)*@\\w+([.-]?\\w+)*(\\.\\w{2,3})+$/.test(str); };".to_string(),
            "function parseJson(str) { try { return JSON.parse(str); } catch (e) { return null; } }".to_string(),
        ];

        let scores = score_pairs("validate email address format", &docs)
            .expect("real ONNX smoke test should initialize and score");

        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|score| score.is_finite()));
        assert!(
            scores[0] > scores[1],
            "expected relevant doc to outrank distractor, got scores {scores:?}"
        );
    }
}
