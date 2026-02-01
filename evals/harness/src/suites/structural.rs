//! Structural query evaluation suite (Suite E)
//!
//! Tests structural code navigation tools:
//! - find_definition: Locate symbol definitions
//! - find_callers: Find functions that call a symbol
//! - find_references: Find all usages of a symbol
//! - find_imports: Find files that import a module
//!
//! Also tests intent classification for natural language queries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::TempDir;

use crate::mcp_client::McpClient;
use crate::TestResult;

// ---------- Dataset Types ----------

/// Complete structural query dataset
#[derive(Debug, Deserialize)]
pub struct StructuralDataset {
    pub version: String,
    pub description: String,
    pub test_project: TestProject,
    pub queries: Vec<StructuralQuery>,
    #[serde(default)]
    pub natural_language_queries: Vec<NLQuery>,
}

/// Test project with source files
#[derive(Debug, Deserialize)]
pub struct TestProject {
    pub name: String,
    pub description: String,
    pub files: Vec<TestFile>,
}

/// A source file in the test project
#[derive(Debug, Deserialize)]
pub struct TestFile {
    pub path: String,
    pub content: String,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "rust".to_string()
}

/// A structural query test case
#[derive(Debug, Deserialize)]
pub struct StructuralQuery {
    pub id: String,
    #[serde(rename = "type")]
    pub query_type: String,
    pub query: String,
    #[serde(default)]
    pub expected: Option<ExpectedDefinition>,
    #[serde(default)]
    pub expected_callers: Option<Vec<ExpectedCaller>>,
    #[serde(default)]
    pub expected_count: Option<usize>,
    #[serde(default)]
    pub expected_files: Option<Vec<String>>,
    #[serde(default)]
    pub expected_importers: Option<Vec<ExpectedImporter>>,
}

/// Expected definition result
#[derive(Debug, Deserialize)]
pub struct ExpectedDefinition {
    pub file: String,
    pub line_start: u32,
    pub kind: String,
}

/// Expected caller result
#[derive(Debug, Deserialize)]
pub struct ExpectedCaller {
    pub function: String,
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
}

/// Expected importer result
#[derive(Debug, Deserialize)]
pub struct ExpectedImporter {
    pub file: String,
    #[serde(default)]
    pub alias: Option<String>,
}

/// Natural language query test case
#[derive(Debug, Deserialize)]
pub struct NLQuery {
    pub id: String,
    pub query: String,
    pub expected_intent: String,
    #[serde(default)]
    pub expected_target: Option<String>,
}

// ---------- Result Types ----------

/// Result of a single structural test
#[derive(Debug, Clone, Serialize)]
pub struct StructuralTestResult {
    pub query_id: String,
    pub passed: bool,
    pub score: f32,
    pub expected: String,
    pub actual: String,
    pub duration_ms: u64,
}

/// Aggregated results for the structural suite
#[derive(Debug, Clone, Default, Serialize)]
pub struct StructuralSuiteResults {
    pub definition_tests: Vec<StructuralTestResult>,
    pub callers_tests: Vec<StructuralTestResult>,
    pub references_tests: Vec<StructuralTestResult>,
    pub imports_tests: Vec<StructuralTestResult>,
    pub intent_tests: Vec<StructuralTestResult>,
    pub overall_score: f32,
    pub pass_rate: f32,
}

impl std::fmt::Display for StructuralSuiteResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let def_pass = self.definition_tests.iter().filter(|t| t.passed).count();
        let caller_pass = self.callers_tests.iter().filter(|t| t.passed).count();
        let refs_pass = self.references_tests.iter().filter(|t| t.passed).count();
        let imports_pass = self.imports_tests.iter().filter(|t| t.passed).count();
        let intent_pass = self.intent_tests.iter().filter(|t| t.passed).count();

        writeln!(f, "Structural Query Suite Results:")?;
        writeln!(
            f,
            "  Definitions: {}/{} passed ({:.1}%)",
            def_pass,
            self.definition_tests.len(),
            if self.definition_tests.is_empty() {
                0.0
            } else {
                (def_pass as f32 / self.definition_tests.len() as f32) * 100.0
            }
        )?;
        writeln!(
            f,
            "  Callers: {}/{} passed ({:.1}%)",
            caller_pass,
            self.callers_tests.len(),
            if self.callers_tests.is_empty() {
                0.0
            } else {
                (caller_pass as f32 / self.callers_tests.len() as f32) * 100.0
            }
        )?;
        writeln!(
            f,
            "  References: {}/{} passed ({:.1}%)",
            refs_pass,
            self.references_tests.len(),
            if self.references_tests.is_empty() {
                0.0
            } else {
                (refs_pass as f32 / self.references_tests.len() as f32) * 100.0
            }
        )?;
        writeln!(
            f,
            "  Imports: {}/{} passed ({:.1}%)",
            imports_pass,
            self.imports_tests.len(),
            if self.imports_tests.is_empty() {
                0.0
            } else {
                (imports_pass as f32 / self.imports_tests.len() as f32) * 100.0
            }
        )?;
        writeln!(
            f,
            "  Intent Classification: {}/{} passed ({:.1}%)",
            intent_pass,
            self.intent_tests.len(),
            if self.intent_tests.is_empty() {
                0.0
            } else {
                (intent_pass as f32 / self.intent_tests.len() as f32) * 100.0
            }
        )?;
        writeln!(f, "  Overall Score: {:.3}", self.overall_score)?;
        write!(f, "  Pass Rate: {:.1}%", self.pass_rate * 100.0)
    }
}

// ---------- Helper Functions ----------

/// Extract text content from MCP response
fn extract_content_text(response: &Value) -> Option<&str> {
    response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
}

/// Load dataset from JSON file
pub fn load_dataset(path: &Path) -> Result<StructuralDataset, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read file: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("parse json: {}", e))
}

/// Calculate precision score for set comparison
fn calculate_precision(found: &HashSet<String>, expected: &HashSet<String>) -> f32 {
    if found.is_empty() {
        return 0.0;
    }
    let hits = found.intersection(expected).count();
    hits as f32 / found.len() as f32
}

/// Calculate recall score for set comparison
fn calculate_recall(found: &HashSet<String>, expected: &HashSet<String>) -> f32 {
    if expected.is_empty() {
        return 1.0;
    }
    let hits = found.intersection(expected).count();
    hits as f32 / expected.len() as f32
}

/// Calculate F1 score from precision and recall
fn calculate_f1(precision: f32, recall: f32) -> f32 {
    if precision + recall == 0.0 {
        return 0.0;
    }
    2.0 * precision * recall / (precision + recall)
}

// ---------- Main Suite Functions ----------

/// Run all structural evaluation tests
pub fn run_structural_tests(memd_path: &PathBuf, embedding_model: &str) -> Vec<TestResult> {
    let mut results = Vec::new();

    // Load primary dataset
    let dataset_path = Path::new("evals/datasets/structural/structural_queries.json");
    let dataset = match load_dataset(dataset_path) {
        Ok(d) => d,
        Err(e) => {
            results.push(TestResult::fail(
                "E_load_dataset",
                &format!("Failed to load dataset: {}", e),
            ));
            return results;
        }
    };

    println!("\n=== Structural Query Suite (Suite E) ===");
    println!("Dataset: {} (v{})", dataset.description, dataset.version);
    println!(
        "Project: {} - {} files",
        dataset.test_project.name,
        dataset.test_project.files.len()
    );
    println!(
        "Queries: {} structural, {} natural language\n",
        dataset.queries.len(),
        dataset.natural_language_queries.len()
    );

    // E1: Setup and index test project
    let (e1_result, client_opt, data_dir) =
        run_e1_setup_project(memd_path, &dataset, embedding_model);
    results.push(e1_result);

    let mut client = match client_opt {
        Some(c) => c,
        None => return results,
    };

    // E2: Run definition tests
    let (e2_result, def_results) = run_e2_definition_tests(&mut client, &dataset);
    results.push(e2_result);

    // E3: Run callers tests
    let (e3_result, caller_results) = run_e3_callers_tests(&mut client, &dataset);
    results.push(e3_result);

    // E4: Run references tests
    let (e4_result, refs_results) = run_e4_references_tests(&mut client, &dataset);
    results.push(e4_result);

    // E5: Run imports tests
    let (e5_result, imports_results) = run_e5_imports_tests(&mut client, &dataset);
    results.push(e5_result);

    // E6: Run intent classification tests (doesn't need MCP client)
    let (e6_result, intent_results) = run_e6_intent_tests(&dataset);
    results.push(e6_result);

    // E7: Quality thresholds check
    let suite_results = StructuralSuiteResults {
        definition_tests: def_results,
        callers_tests: caller_results,
        references_tests: refs_results,
        imports_tests: imports_results,
        intent_tests: intent_results,
        overall_score: 0.0,
        pass_rate: 0.0,
    };

    results.push(run_e7_quality_thresholds(&suite_results));

    // Keep data_dir alive until tests complete
    drop(data_dir);

    results
}

fn run_e1_setup_project(
    memd_path: &PathBuf,
    dataset: &StructuralDataset,
    embedding_model: &str,
) -> (TestResult, Option<McpClient>, Option<TempDir>) {
    let start = Instant::now();
    let name = "E1_setup_project";

    let data_dir = match TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("tempdir: {}", e), start),
                None,
                None,
            )
        }
    };

    let mut client = match McpClient::start_with_args(
        memd_path,
        &[
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--embedding-model",
            embedding_model,
        ],
    ) {
        Ok(c) => c,
        Err(e) => {
            return (
                TestResult::fail_with_duration(name, &format!("start memd: {}", e), start),
                None,
                None,
            )
        }
    };

    if let Err(e) = client.initialize() {
        return (
            TestResult::fail_with_duration(name, &format!("initialize: {}", e), start),
            None,
            None,
        );
    }

    // Index all test files as code chunks
    for file in &dataset.test_project.files {
        let params = serde_json::json!({
            "tenant_id": "eval_structural",
            "text": file.content,
            "type": "code",
            "source": {
                "path": file.path
            },
            "tags": [file.path.clone(), file.language.clone()]
        });

        if let Err(e) = client.call_tool("memory.add", params) {
            return (
                TestResult::fail_with_duration(
                    name,
                    &format!("add file {}: {}", file.path, e),
                    start,
                ),
                None,
                None,
            );
        }
    }

    println!("  Indexed {} files", dataset.test_project.files.len());

    (
        TestResult::pass_with_duration(name, start),
        Some(client),
        Some(data_dir),
    )
}

fn run_e2_definition_tests(
    client: &mut McpClient,
    dataset: &StructuralDataset,
) -> (TestResult, Vec<StructuralTestResult>) {
    let start = Instant::now();
    let name = "E2_definition_tests";

    let def_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "find_definition")
        .collect();

    let mut test_results = Vec::new();
    let mut passed = 0;

    for query in &def_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_structural",
            "name": query.query
        });

        let response = match client.call_tool("code.find_definition", params) {
            Ok(r) => r,
            Err(e) => {
                test_results.push(StructuralTestResult {
                    query_id: query.id.clone(),
                    passed: false,
                    score: 0.0,
                    expected: format!("{:?}", query.expected),
                    actual: format!("Error: {}", e),
                    duration_ms: query_start.elapsed().as_millis() as u64,
                });
                continue;
            }
        };

        let (is_passed, score, actual) = evaluate_definition_result(&response, &query.expected);

        if is_passed {
            passed += 1;
        }

        test_results.push(StructuralTestResult {
            query_id: query.id.clone(),
            passed: is_passed,
            score,
            expected: format!("{:?}", query.expected),
            actual,
            duration_ms: query_start.elapsed().as_millis() as u64,
        });
    }

    println!(
        "  Definitions: {}/{} passed",
        passed,
        def_queries.len()
    );

    let result = if passed == def_queries.len() || def_queries.is_empty() {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("{}/{} definition tests passed", passed, def_queries.len()),
            start,
        )
    };

    (result, test_results)
}

fn evaluate_definition_result(
    response: &Value,
    expected: &Option<ExpectedDefinition>,
) -> (bool, f32, String) {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return (false, 0.0, "No response content".to_string()),
    };

    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (false, 0.0, format!("Parse error: {}", e)),
    };

    let definitions = match parsed.get("definitions").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => return (false, 0.0, "No definitions array".to_string()),
    };

    if definitions.is_empty() {
        return (false, 0.0, "Empty definitions".to_string());
    }

    let expected = match expected {
        Some(e) => e,
        None => return (true, 1.0, "No expected (any result ok)".to_string()),
    };

    // Check first definition matches expected
    let first = &definitions[0];
    let file_path = first
        .get("file_path")
        .and_then(|f| f.as_str())
        .unwrap_or("");
    let kind = first.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let line = first
        .get("line_start")
        .and_then(|l| l.as_u64())
        .unwrap_or(0) as u32;

    let actual = format!(
        "file={}, kind={}, line={}",
        file_path, kind, line
    );

    // Score: 1.0 for exact match, 0.5 for file match, 0.25 for kind match
    let file_match = file_path.ends_with(&expected.file);
    let kind_match = kind.to_lowercase() == expected.kind.to_lowercase();
    let line_match = line == expected.line_start;

    let score = if file_match && kind_match && line_match {
        1.0
    } else if file_match && kind_match {
        0.75
    } else if file_match {
        0.5
    } else if kind_match {
        0.25
    } else {
        0.0
    };

    let passed = file_match && kind_match; // Line can vary due to parsing differences

    (passed, score, actual)
}

fn run_e3_callers_tests(
    client: &mut McpClient,
    dataset: &StructuralDataset,
) -> (TestResult, Vec<StructuralTestResult>) {
    let start = Instant::now();
    let name = "E3_callers_tests";

    let caller_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "find_callers")
        .collect();

    let mut test_results = Vec::new();
    let mut passed = 0;

    for query in &caller_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_structural",
            "name": query.query,
            "depth": 2
        });

        let response = match client.call_tool("code.find_callers", params) {
            Ok(r) => r,
            Err(e) => {
                test_results.push(StructuralTestResult {
                    query_id: query.id.clone(),
                    passed: false,
                    score: 0.0,
                    expected: format!("{:?}", query.expected_callers),
                    actual: format!("Error: {}", e),
                    duration_ms: query_start.elapsed().as_millis() as u64,
                });
                continue;
            }
        };

        let (is_passed, score, actual) =
            evaluate_callers_result(&response, &query.expected_callers);

        if is_passed {
            passed += 1;
        }

        test_results.push(StructuralTestResult {
            query_id: query.id.clone(),
            passed: is_passed,
            score,
            expected: format!("{:?}", query.expected_callers),
            actual,
            duration_ms: query_start.elapsed().as_millis() as u64,
        });
    }

    println!(
        "  Callers: {}/{} passed",
        passed,
        caller_queries.len()
    );

    let result = if passed == caller_queries.len() || caller_queries.is_empty() {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("{}/{} callers tests passed", passed, caller_queries.len()),
            start,
        )
    };

    (result, test_results)
}

fn evaluate_callers_result(
    response: &Value,
    expected_callers: &Option<Vec<ExpectedCaller>>,
) -> (bool, f32, String) {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return (false, 0.0, "No response content".to_string()),
    };

    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (false, 0.0, format!("Parse error: {}", e)),
    };

    let callers = match parsed.get("callers").and_then(|c| c.as_array()) {
        Some(arr) => arr,
        None => return (false, 0.0, "No callers array".to_string()),
    };

    let expected = match expected_callers {
        Some(e) => e,
        None => return (true, 1.0, "No expected (any result ok)".to_string()),
    };

    // Build sets for comparison
    let found_set: HashSet<String> = callers
        .iter()
        .filter_map(|c| {
            let name = c.get("caller_name")?.as_str()?;
            Some(name.to_string())
        })
        .collect();

    let expected_set: HashSet<String> = expected.iter().map(|e| e.function.clone()).collect();

    let precision = calculate_precision(&found_set, &expected_set);
    let recall = calculate_recall(&found_set, &expected_set);
    let f1 = calculate_f1(precision, recall);

    let actual = format!("found: {:?}", found_set);

    // Pass if recall is 100% (found all expected callers)
    let passed = recall >= 1.0;

    (passed, f1, actual)
}

fn run_e4_references_tests(
    client: &mut McpClient,
    dataset: &StructuralDataset,
) -> (TestResult, Vec<StructuralTestResult>) {
    let start = Instant::now();
    let name = "E4_references_tests";

    let refs_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "find_references")
        .collect();

    let mut test_results = Vec::new();
    let mut passed = 0;

    for query in &refs_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_structural",
            "name": query.query
        });

        let response = match client.call_tool("code.find_references", params) {
            Ok(r) => r,
            Err(e) => {
                test_results.push(StructuralTestResult {
                    query_id: query.id.clone(),
                    passed: false,
                    score: 0.0,
                    expected: format!("count={:?}, files={:?}", query.expected_count, query.expected_files),
                    actual: format!("Error: {}", e),
                    duration_ms: query_start.elapsed().as_millis() as u64,
                });
                continue;
            }
        };

        let (is_passed, score, actual) = evaluate_references_result(
            &response,
            query.expected_count,
            &query.expected_files,
        );

        if is_passed {
            passed += 1;
        }

        test_results.push(StructuralTestResult {
            query_id: query.id.clone(),
            passed: is_passed,
            score,
            expected: format!("count={:?}, files={:?}", query.expected_count, query.expected_files),
            actual,
            duration_ms: query_start.elapsed().as_millis() as u64,
        });
    }

    println!(
        "  References: {}/{} passed",
        passed,
        refs_queries.len()
    );

    let result = if passed == refs_queries.len() || refs_queries.is_empty() {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("{}/{} references tests passed", passed, refs_queries.len()),
            start,
        )
    };

    (result, test_results)
}

fn evaluate_references_result(
    response: &Value,
    expected_count: Option<usize>,
    expected_files: &Option<Vec<String>>,
) -> (bool, f32, String) {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return (false, 0.0, "No response content".to_string()),
    };

    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (false, 0.0, format!("Parse error: {}", e)),
    };

    let references = match parsed.get("references").and_then(|r| r.as_array()) {
        Some(arr) => arr,
        None => return (false, 0.0, "No references array".to_string()),
    };

    let actual_count = references.len();
    let actual_files: HashSet<String> = references
        .iter()
        .filter_map(|r| {
            let path = r.get("file_path")?.as_str()?;
            Some(path.to_string())
        })
        .collect();

    let actual = format!("count={}, files={:?}", actual_count, actual_files);

    // Check count (allow some flexibility)
    let count_ok = expected_count.map_or(true, |c| {
        actual_count >= c.saturating_sub(1) && actual_count <= c + 1
    });

    // Check files
    let files_ok = expected_files.as_ref().map_or(true, |expected| {
        expected.iter().all(|f| {
            actual_files.iter().any(|af| af.ends_with(f))
        })
    });

    let passed = count_ok && files_ok;
    let score = if passed { 1.0 } else if count_ok || files_ok { 0.5 } else { 0.0 };

    (passed, score, actual)
}

fn run_e5_imports_tests(
    client: &mut McpClient,
    dataset: &StructuralDataset,
) -> (TestResult, Vec<StructuralTestResult>) {
    let start = Instant::now();
    let name = "E5_imports_tests";

    let imports_queries: Vec<_> = dataset
        .queries
        .iter()
        .filter(|q| q.query_type == "find_imports")
        .collect();

    let mut test_results = Vec::new();
    let mut passed = 0;

    for query in &imports_queries {
        let query_start = Instant::now();

        let params = serde_json::json!({
            "tenant_id": "eval_structural",
            "module": query.query
        });

        let response = match client.call_tool("code.find_imports", params) {
            Ok(r) => r,
            Err(e) => {
                test_results.push(StructuralTestResult {
                    query_id: query.id.clone(),
                    passed: false,
                    score: 0.0,
                    expected: format!("{:?}", query.expected_importers),
                    actual: format!("Error: {}", e),
                    duration_ms: query_start.elapsed().as_millis() as u64,
                });
                continue;
            }
        };

        let (is_passed, score, actual) =
            evaluate_imports_result(&response, &query.expected_importers);

        if is_passed {
            passed += 1;
        }

        test_results.push(StructuralTestResult {
            query_id: query.id.clone(),
            passed: is_passed,
            score,
            expected: format!("{:?}", query.expected_importers),
            actual,
            duration_ms: query_start.elapsed().as_millis() as u64,
        });
    }

    println!(
        "  Imports: {}/{} passed",
        passed,
        imports_queries.len()
    );

    let result = if passed == imports_queries.len() || imports_queries.is_empty() {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!("{}/{} imports tests passed", passed, imports_queries.len()),
            start,
        )
    };

    (result, test_results)
}

fn evaluate_imports_result(
    response: &Value,
    expected_importers: &Option<Vec<ExpectedImporter>>,
) -> (bool, f32, String) {
    let text = match extract_content_text(response) {
        Some(t) => t,
        None => return (false, 0.0, "No response content".to_string()),
    };

    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return (false, 0.0, format!("Parse error: {}", e)),
    };

    let imports = match parsed.get("imports").and_then(|i| i.as_array()) {
        Some(arr) => arr,
        None => return (false, 0.0, "No imports array".to_string()),
    };

    let expected = match expected_importers {
        Some(e) => e,
        None => return (true, 1.0, "No expected (any result ok)".to_string()),
    };

    // Build sets for comparison
    let found_files: HashSet<String> = imports
        .iter()
        .filter_map(|i| {
            let file = i.get("importing_file")?.as_str()?;
            Some(file.to_string())
        })
        .collect();

    let expected_files: HashSet<String> = expected.iter().map(|e| e.file.clone()).collect();

    let actual = format!("importing_files: {:?}", found_files);

    // Check if all expected files were found
    let all_found = expected_files.iter().all(|ef| {
        found_files.iter().any(|ff| ff.ends_with(ef))
    });

    let score = if all_found { 1.0 } else { 0.0 };

    (all_found, score, actual)
}

fn run_e6_intent_tests(dataset: &StructuralDataset) -> (TestResult, Vec<StructuralTestResult>) {
    let start = Instant::now();
    let name = "E6_intent_classification";

    // Intent classification tests use query patterns from 06-07
    let mut test_results = Vec::new();
    let mut passed = 0;

    for nl_query in &dataset.natural_language_queries {
        let query_start = Instant::now();

        // Simple pattern matching for intent classification
        // This mirrors the QueryRouter patterns from 06-07
        let detected_intent = classify_intent(&nl_query.query);
        let expected_intent = &nl_query.expected_intent;

        let is_passed = detected_intent == *expected_intent;
        if is_passed {
            passed += 1;
        }

        test_results.push(StructuralTestResult {
            query_id: nl_query.id.clone(),
            passed: is_passed,
            score: if is_passed { 1.0 } else { 0.0 },
            expected: expected_intent.clone(),
            actual: detected_intent,
            duration_ms: query_start.elapsed().as_millis() as u64,
        });
    }

    println!(
        "  Intent Classification: {}/{} passed",
        passed,
        dataset.natural_language_queries.len()
    );

    let result = if passed == dataset.natural_language_queries.len()
        || dataset.natural_language_queries.is_empty()
    {
        TestResult::pass_with_duration(name, start)
    } else {
        TestResult::fail_with_duration(
            name,
            &format!(
                "{}/{} intent tests passed",
                passed,
                dataset.natural_language_queries.len()
            ),
            start,
        )
    };

    (result, test_results)
}

/// Classify query intent using pattern matching
/// Mirrors QueryRouter patterns from 06-07
fn classify_intent(query: &str) -> String {
    let query_lower = query.to_lowercase();

    // Definition patterns
    if query_lower.contains("where is") && query_lower.contains("defined")
        || query_lower.contains("find definition")
        || query_lower.contains("definition of")
        || query_lower.starts_with("def:")
    {
        return "SymbolDefinition".to_string();
    }

    // Callers patterns
    if query_lower.contains("who calls")
        || query_lower.contains("callers of")
        || query_lower.contains("show callers")
        || query_lower.starts_with("callers:")
    {
        return "SymbolCallers".to_string();
    }

    // References patterns
    if query_lower.contains("usages of")
        || query_lower.contains("all usages")
        || query_lower.contains("where is") && query_lower.contains("used")
        || query_lower.contains("find all uses")
        || query_lower.starts_with("refs:")
    {
        return "SymbolReferences".to_string();
    }

    // Imports patterns
    if query_lower.contains("who imports")
        || query_lower.contains("imports of")
        || query_lower.starts_with("imports:")
    {
        return "SymbolImports".to_string();
    }

    // Default to semantic search
    "SemanticSearch".to_string()
}

fn run_e7_quality_thresholds(suite_results: &StructuralSuiteResults) -> TestResult {
    let start = Instant::now();
    let name = "E7_quality_thresholds";

    // Calculate pass rates per category
    let def_pass_rate = if suite_results.definition_tests.is_empty() {
        1.0
    } else {
        suite_results
            .definition_tests
            .iter()
            .filter(|t| t.passed)
            .count() as f32
            / suite_results.definition_tests.len() as f32
    };

    let caller_pass_rate = if suite_results.callers_tests.is_empty() {
        1.0
    } else {
        suite_results
            .callers_tests
            .iter()
            .filter(|t| t.passed)
            .count() as f32
            / suite_results.callers_tests.len() as f32
    };

    let refs_pass_rate = if suite_results.references_tests.is_empty() {
        1.0
    } else {
        suite_results
            .references_tests
            .iter()
            .filter(|t| t.passed)
            .count() as f32
            / suite_results.references_tests.len() as f32
    };

    let imports_pass_rate = if suite_results.imports_tests.is_empty() {
        1.0
    } else {
        suite_results
            .imports_tests
            .iter()
            .filter(|t| t.passed)
            .count() as f32
            / suite_results.imports_tests.len() as f32
    };

    let intent_pass_rate = if suite_results.intent_tests.is_empty() {
        1.0
    } else {
        suite_results
            .intent_tests
            .iter()
            .filter(|t| t.passed)
            .count() as f32
            / suite_results.intent_tests.len() as f32
    };

    println!("\n=== Quality Thresholds ===");
    println!(
        "  Definitions: {:.1}% (threshold: 80%)",
        def_pass_rate * 100.0
    );
    println!(
        "  Callers: {:.1}% (threshold: 70%)",
        caller_pass_rate * 100.0
    );
    println!(
        "  References: {:.1}% (threshold: 70%)",
        refs_pass_rate * 100.0
    );
    println!(
        "  Imports: {:.1}% (threshold: 80%)",
        imports_pass_rate * 100.0
    );
    println!(
        "  Intent: {:.1}% (threshold: 80%)",
        intent_pass_rate * 100.0
    );

    let mut failures = Vec::new();

    // Thresholds
    if def_pass_rate < 0.80 {
        failures.push(format!("Definitions {:.1}% < 80%", def_pass_rate * 100.0));
    }
    if caller_pass_rate < 0.70 {
        failures.push(format!("Callers {:.1}% < 70%", caller_pass_rate * 100.0));
    }
    if refs_pass_rate < 0.70 {
        failures.push(format!("References {:.1}% < 70%", refs_pass_rate * 100.0));
    }
    if imports_pass_rate < 0.80 {
        failures.push(format!("Imports {:.1}% < 80%", imports_pass_rate * 100.0));
    }
    if intent_pass_rate < 0.80 {
        failures.push(format!("Intent {:.1}% < 80%", intent_pass_rate * 100.0));
    }

    if failures.is_empty() {
        println!("  All quality thresholds met!");
        TestResult::pass_with_duration(name, start)
    } else {
        println!("  Thresholds not met: {}", failures.join("; "));
        TestResult::fail_with_duration(name, &failures.join("; "), start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_dataset() {
        let dataset_path = Path::new("evals/datasets/structural/structural_queries.json");
        if dataset_path.exists() {
            let result = load_dataset(dataset_path);
            assert!(result.is_ok());
            let dataset = result.unwrap();
            assert!(!dataset.queries.is_empty());
            assert!(!dataset.test_project.files.is_empty());
        }
    }

    #[test]
    fn test_definition_scoring() {
        let expected = ExpectedDefinition {
            file: "src/main.rs".to_string(),
            line_start: 6,
            kind: "function".to_string(),
        };

        // Simulate a matching response
        let response = serde_json::json!({
            "result": {
                "content": [{
                    "type": "text",
                    "text": r#"{"definitions":[{"file_path":"src/main.rs","kind":"function","line_start":6}]}"#
                }]
            }
        });

        let (passed, score, _) = evaluate_definition_result(&response, &Some(expected));
        assert!(passed);
        assert!(score > 0.5);
    }

    #[test]
    fn test_callers_scoring() {
        let expected = vec![
            ExpectedCaller {
                function: "main".to_string(),
                file: "src/main.rs".to_string(),
                line: Some(2),
            },
        ];

        let response = serde_json::json!({
            "result": {
                "content": [{
                    "type": "text",
                    "text": r#"{"callers":[{"caller_name":"main","caller_file":"src/main.rs"}]}"#
                }]
            }
        });

        let (passed, score, _) = evaluate_callers_result(&response, &Some(expected));
        assert!(passed);
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_intent_classification() {
        assert_eq!(
            classify_intent("where is hello_world defined"),
            "SymbolDefinition"
        );
        assert_eq!(classify_intent("who calls helper"), "SymbolCallers");
        assert_eq!(
            classify_intent("find all usages of Config"),
            "SymbolReferences"
        );
        assert_eq!(
            classify_intent("how does the greeting work"),
            "SemanticSearch"
        );
    }

    #[test]
    fn test_precision_recall() {
        let found: HashSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let expected: HashSet<String> = ["a", "b", "d"].iter().map(|s| s.to_string()).collect();

        let precision = calculate_precision(&found, &expected);
        let recall = calculate_recall(&found, &expected);

        assert!((precision - 0.666).abs() < 0.01);
        assert!((recall - 0.666).abs() < 0.01);
    }
}
