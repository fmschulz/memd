# Codebase Indexing Example

Index and search codebases semantically using memd's hybrid retrieval.

## Scenario

You have a Rust web service and want to make the entire codebase searchable for the AI agent.

## Step 1: Batch Index Repository

```bash
#!/bin/bash
# index-codebase.sh

TENANT_ID="web-service-backend"
REPO_ROOT="/home/user/projects/web-service"

echo "Indexing codebase into memd..."

# Use memd CLI or MCP client to batch index
find "$REPO_ROOT" -type f -name "*.rs" | while read -r file; do
  # Get relative path
  rel_path="${file#$REPO_ROOT/}"

  # Determine tags from path
  if [[ "$rel_path" == src/api/* ]]; then
    tags="rust,api,endpoints"
  elif [[ "$rel_path" == src/models/* ]]; then
    tags="rust,models,database"
  elif [[ "$rel_path" == src/auth/* ]]; then
    tags="rust,auth,security"
  elif [[ "$rel_path" == tests/* ]]; then
    tags="rust,tests"
  else
    tags="rust,core"
  fi

  # Read file content
  content=$(cat "$file")

  # Add to memd (pseudo-code - actual implementation via MCP)
  echo "Indexing $rel_path..."
  # memory.add with content, path as metadata, appropriate tags
done

echo "Codebase indexing complete!"
```

## Step 2: Semantic Code Search

```markdown
User: "Find all functions that interact with the database"

Agent: Searching codebase memory...

<memory.search>
{
  "tenant_id": "web-service-backend",
  "query": "database interaction query execution connection",
  "k": 20
}

Results:
1. src/models/user.rs - "impl User { async fn find_by_id(pool: &PgPool, id: i64) -> Result<User> { sqlx::query_as!(User, \"SELECT * FROM users WHERE id = $1\", id).fetch_one(pool).await } }"

2. src/db/connection.rs - "pub async fn establish_connection() -> Result<PgPool> { PgPoolOptions::new().max_connections(5).connect(&env::var(\"DATABASE_URL\")?).await }"

3. src/api/users.rs - "async fn get_user(pool: web::Data<PgPool>, user_id: web::Path<i64>) -> HttpResponse { match User::find_by_id(&pool, *user_id).await { Ok(user) => HttpResponse::Ok().json(user), Err(_) => HttpResponse::NotFound().finish() } }"

Agent: Found 20 database-related code snippets across models, connection handling, and API endpoints.
```

## Step 3: Code Navigation

```markdown
User: "Show me how the User model is used throughout the codebase"

Agent: Using structural code queries...

<code.find_definition>
{
  "tenant_id": "web-service-backend",
  "symbol_name": "User"
}

Result: "src/models/user.rs:15 - struct User { id: i64, email: String, password_hash: String, created_at: DateTime<Utc> }"

<code.find_references>
{
  "tenant_id": "web-service-backend",
  "symbol_name": "User"
}

Results:
1. src/api/users.rs:23 - Import: use crate::models::User;
2. src/api/users.rs:45 - Usage: User::find_by_id(&pool, id)
3. src/api/auth.rs:67 - Usage: User::create(&pool, new_user)
4. tests/integration/user_tests.rs:12 - Usage: User { id: 1, ... }

Agent: The User model is defined in models/user.rs and used in API handlers and tests. Found 15 references across the codebase.
```

## Step 4: Find Similar Code Patterns

```markdown
User: "Find all API endpoints that follow the same pattern as the get_user handler"

Agent: Searching for similar patterns...

<memory.search>
{
  "tenant_id": "web-service-backend",
  "query": "async fn api handler HttpResponse web::Path extract parameter",
  "k": 15
}

Results:
1. src/api/users.rs - "async fn get_user(pool: web::Data<PgPool>, user_id: web::Path<i64>) -> HttpResponse"
2. src/api/posts.rs - "async fn get_post(pool: web::Data<PgPool>, post_id: web::Path<i64>) -> HttpResponse"
3. src/api/comments.rs - "async fn get_comment(pool: web::Data<PgPool>, comment_id: web::Path<i64>) -> HttpResponse"

Agent: Found 12 similar API handler patterns all following the same structure:
- Async function
- Database pool injection
- Path parameter extraction
- HttpResponse return type
```

## Step 5: Architecture Understanding

```markdown
User: "Explain the authentication flow in this codebase"

Agent: Let me search for auth-related code...

<memory.search>
{
  "tenant_id": "web-service-backend",
  "query": "authentication login jwt token validation middleware",
  "k": 25
}

<code.find_definition>
{
  "tenant_id": "web-service-backend",
  "symbol_name": "AuthMiddleware"
}

<code.find_callers>
{
  "tenant_id": "web-service-backend",
  "function_name": "validate_token",
  "max_depth": 3
}

Agent: Based on memory and code structure:

1. **Login Flow** (src/api/auth.rs:34):
   - User submits credentials
   - validate_credentials() checks against database
   - generate_jwt() creates access token
   - Returns token in JSON response

2. **Token Validation** (src/middleware/auth.rs:15):
   - AuthMiddleware intercepts requests
   - Extracts Bearer token from Authorization header
   - validate_token() verifies signature and expiration
   - Injects user claims into request context

3. **Protected Routes** (src/api/config.rs:45):
   - Routes wrapped with .wrap(AuthMiddleware)
   - Automatically enforce authentication
   - User data available via request extensions

Call graph shows validate_token() is called by:
- AuthMiddleware::call() (direct)
- Protected route handlers (via middleware)
- Refresh token endpoint (for renewal)
```

## Performance Characteristics

From benchmarks on 100-file Rust codebase:

- **Indexing**: ~2-3 seconds for full codebase (10K LOC)
- **Search Latency**: 99.5ms p50, 130ms p99
- **Recall@10**: 100% for code similarity queries
- **Storage**: ~5MB for 100 files with embeddings

## Tips for Effective Indexing

1. **Granular Tagging** - Tag by module, feature, language
2. **File-Level Metadata** - Include file paths in chunk metadata
3. **Function-Level Chunks** - Index individual functions for precision
4. **Update Strategy** - Re-index modified files on git commit
5. **Tenant Per Project** - Use separate tenant_id for each repository
6. **Hybrid Queries** - Combine semantic search with structural queries

## Integration with Git Hooks

```bash
#!/bin/bash
# .git/hooks/post-commit

# Re-index modified files after commit
git diff-tree --no-commit-id --name-only -r HEAD | \
  grep '\.rs$' | \
  while read file; do
    echo "Re-indexing $file..."
    # Call memd to update this file's chunks
  done
```

## Batch Indexing Script

```python
#!/usr/bin/env python3
# batch-index.py

import os
import json
import subprocess
from pathlib import Path

TENANT_ID = "my-rust-project"
REPO_ROOT = Path("/path/to/repo")

def index_file(file_path):
    """Index a single file into memd"""
    rel_path = file_path.relative_to(REPO_ROOT)

    # Determine tags from path
    tags = ["rust"]
    if "src/api" in str(rel_path):
        tags.extend(["api", "endpoints"])
    elif "src/models" in str(rel_path):
        tags.extend(["models", "database"])
    elif "tests" in str(rel_path):
        tags.append("tests")

    # Read file content
    with open(file_path, 'r') as f:
        content = f.read()

    # Create MCP request (simplified)
    request = {
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "memory.add",
            "arguments": {
                "tenant_id": TENANT_ID,
                "text": content,
                "chunk_type": "code",
                "tags": tags,
                "metadata": {
                    "file_path": str(rel_path),
                    "file_name": file_path.name
                }
            }
        },
        "id": 1
    }

    # Send to memd via stdio (pseudo-code)
    # In practice, use MCP client library
    print(f"Indexed: {rel_path}")

def main():
    # Find all Rust files
    rust_files = list(REPO_ROOT.glob("**/*.rs"))

    print(f"Found {len(rust_files)} Rust files to index...")

    for file_path in rust_files:
        index_file(file_path)

    print("Indexing complete!")

if __name__ == "__main__":
    main()
```

This example demonstrates how memd transforms codebase navigation from keyword-based to semantic understanding.
