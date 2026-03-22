# Codebase Indexing with Task Tracking

This example shows how to index a repository into raw `memory.*` chunks while also tracking the indexing job itself as a structured task.

## Scenario

Tenant: `web-service-backend`

Use case:

- raw source files should be searchable as memory chunks
- the indexing job should still record motivation, parameters, coverage, and failures

That means:

- `memory.add_batch` stores code/document chunks
- `task.*` stores the knowledge artifact history of the indexing workflow

## Step 1: Start the Indexing Task

```json
{
  "name": "task.start",
  "arguments": {
    "tenant_id": "web-service-backend",
    "project_id": "repo-index",
    "goal": "Index the repository for cross-agent code search",
    "motivation": "Agents need shared searchable access to code patterns, definitions, and architecture context",
    "hypothesis": "Batch indexing source files with path metadata and tags will provide enough retrieval quality for code understanding",
    "scientific_question": "What indexing approach gives useful search coverage without excessive ingest cost?",
    "dataset_refs": [
      {"name": "repository_snapshot", "version": "HEAD"}
    ],
    "expected_outputs": [
      "indexed code chunks",
      "coverage summary",
      "follow-up gaps"
    ]
  }
}
```

## Step 2: Record the Indexing Run

```json
{
  "name": "task.run_start",
  "arguments": {
    "tenant_id": "web-service-backend",
    "task_id": "<task_id>",
    "project_id": "repo-index",
    "tool_name": "index-codebase.sh",
    "command": "./scripts/index-codebase.sh",
    "why_chosen": "Need reproducible batch ingest with file-path-derived tags",
    "parameters": {
      "languages": ["rust"],
      "batch_size": 200
    },
    "inputs": [
      "src/**/*.rs",
      "tests/**/*.rs",
      "README.md"
    ]
  }
}
```

## Step 3: Store Raw Chunks

The indexing script should still use `memory.add_batch` for raw chunks. The task artifact does not replace that.

Example batch payload:

```json
{
  "name": "memory.add_batch",
  "arguments": {
    "tenant_id": "web-service-backend",
    "chunks": [
      {
        "text": "pub async fn get_user(pool: web::Data<PgPool>, user_id: web::Path<i64>) -> HttpResponse { ... }",
        "type": "code",
        "project_id": "repo-index",
        "source": {
          "path": "src/api/users.rs",
          "repo": "web-service-backend"
        },
        "tags": ["rust", "api", "ctx:file:src/api/users.rs", "ctx:subsystem:api"]
      },
      {
        "text": "pub async fn establish_connection() -> Result<PgPool> { ... }",
        "type": "code",
        "project_id": "repo-index",
        "source": {
          "path": "src/db/connection.rs",
          "repo": "web-service-backend"
        },
        "tags": ["rust", "database", "ctx:file:src/db/connection.rs", "ctx:subsystem:db"]
      }
    ]
  }
}
```

## Step 4: Finish the Run and Capture Coverage

```json
{
  "name": "task.run_finish",
  "arguments": {
    "tenant_id": "web-service-backend",
    "task_id": "<task_id>",
    "project_id": "repo-index",
    "status": "completed",
    "tool_name": "index-codebase.sh",
    "outputs": [
      "214 files indexed",
      "4 files skipped"
    ],
    "metrics": {
      "files_indexed": 214,
      "files_skipped": 4
    },
    "notes": "Binary assets and generated files were intentionally skipped",
    "validation": [
      "Spot-check searches returned expected API and database code"
    ]
  }
}
```

If there were specific gaps, add them explicitly:

```json
{
  "name": "task.progress",
  "arguments": {
    "tenant_id": "web-service-backend",
    "task_id": "<task_id>",
    "project_id": "repo-index",
    "summary": "Search quality is good for Rust source but weak for deployment docs",
    "blockers": [
      "Markdown docs were not chunked with subsystem tags"
    ],
    "failed_attempts": [
      "A naive docs-only batch caused noisy retrieval without path-derived tags"
    ],
    "next_step": "Add targeted indexing for architecture docs with ctx tags"
  }
}
```

## Step 5: Finish the Task

```json
{
  "name": "task.finish",
  "arguments": {
    "tenant_id": "web-service-backend",
    "task_id": "<task_id>",
    "project_id": "repo-index",
    "what_worked": [
      "Batch ingest of Rust files produced useful code search coverage",
      "Path-derived ctx tags improved subsystem discovery"
    ],
    "what_failed": [
      "Unstructured documentation ingest was too noisy",
      "Generated files added low-value retrieval candidates"
    ],
    "validation": [
      "Searches for API handlers and database helpers returned the expected files"
    ],
    "uncertainty": [
      "Documentation retrieval still needs a better chunking strategy"
    ],
    "followups": [
      "Index architecture docs separately",
      "Add language-specific chunking for frontend files"
    ],
    "confidence": 0.81
  }
}
```

## What Other Agents Gain

Later agents can now do both:

### Search the raw code chunks

```json
{
  "name": "memory.search",
  "arguments": {
    "tenant_id": "web-service-backend",
    "query": "database connection handling",
    "k": 10
  }
}
```

### Search the indexing task history

```json
{
  "name": "task.search",
  "arguments": {
    "tenant_id": "web-service-backend",
    "query": "indexing gaps documentation noise",
    "k": 10,
    "filters": {
      "project_id": "repo-index"
    }
  }
}
```

This is the intended split:

- raw code lives in `memory.*`
- workflow knowledge about the indexing job lives in `task.*`
