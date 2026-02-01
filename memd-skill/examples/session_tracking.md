# Session Context Tracking Example

Track work across multiple coding sessions using memd memory.

## Scenario

You're working on implementing user authentication across multiple sessions. memd helps you maintain context between sessions.

## Session 1: Initial Planning

```markdown
User: "Start working on JWT authentication for the API"

Agent: I'll record this in memory and begin planning.

<memory.add>
{
  "tenant_id": "ecommerce-api",
  "text": "Starting work on JWT authentication feature. Requirements: RS256 signing, 1-hour access tokens, 7-day refresh tokens, role-based claims",
  "chunk_type": "planning",
  "tags": ["auth", "jwt", "in-progress", "session-1"]
}

Agent: Created implementation plan...
```

## Session 2: Implementation Begins

```markdown
User: "Continue the auth work from yesterday"

Agent: Let me search memory for context.

<memory.search>
{
  "tenant_id": "ecommerce-api",
  "query": "authentication work status",
  "k": 5
}

Result: "Starting work on JWT authentication feature. Requirements: RS256 signing..."

Agent: Found context from previous session. I'll continue implementing the JWT service.

<memory.add>
{
  "tenant_id": "ecommerce-api",
  "text": "Implemented JwtService in src/auth/jwt.rs with token generation using RS256. Created key pair in config/keys/. Added tests for token creation and validation.",
  "chunk_type": "code",
  "tags": ["auth", "jwt", "implemented", "session-2"]
}
```

## Session 3: Bug Fix

```markdown
User: "Getting token expiration errors"

Agent: Let me search for JWT implementation details.

<memory.search>
{
  "tenant_id": "ecommerce-api",
  "query": "JWT token implementation details",
  "k": 3
}

Result: "Implemented JwtService in src/auth/jwt.rs with token generation using RS256..."

<code.find_definition>
{
  "tenant_id": "ecommerce-api",
  "symbol_name": "JwtService"
}

Agent: Found the bug - token expiration check was using UTC but server timezone was EST.

<memory.add>
{
  "tenant_id": "ecommerce-api",
  "text": "Bug fix: JWT expiration validation was comparing UTC timestamps with EST server time. Fixed by normalizing all timestamps to UTC in jwt.rs:145",
  "chunk_type": "bug-fix",
  "tags": ["auth", "jwt", "bug-fix", "expiration", "session-3"]
}
```

## Session 4: Code Review

```markdown
User: "Review the authentication implementation"

Agent: Let me gather all auth-related work from memory.

<memory.search>
{
  "tenant_id": "ecommerce-api",
  "query": "JWT authentication implementation",
  "k": 10
}

Results:
1. "Starting work on JWT authentication feature. Requirements: RS256..."
2. "Implemented JwtService in src/auth/jwt.rs..."
3. "Bug fix: JWT expiration validation..."

Agent: Based on memory, I can see:
- Initial requirements met (RS256, token durations, role claims)
- Bug discovered and fixed related to timezone handling
- All implementation in src/auth/jwt.rs

Let me review the code...
```

## Session 5: Documentation

```markdown
User: "Document the authentication system"

Agent: I'll gather all decisions and implementation details from memory.

<memory.search>
{
  "tenant_id": "ecommerce-api",
  "query": "authentication architecture decisions implementation",
  "k": 15
}

Agent: Created comprehensive documentation from memory context.

<memory.add>
{
  "tenant_id": "ecommerce-api",
  "text": "Completed: JWT authentication system documented in docs/auth.md. Covers architecture, token flows, key rotation, timezone handling bug fix, and usage examples.",
  "chunk_type": "documentation",
  "tags": ["auth", "jwt", "completed", "documented", "session-5"]
}
```

## Benefits Demonstrated

1. **Context Continuity** - Each session starts with full context from previous work
2. **Decision Tracking** - All architectural choices persisted and searchable
3. **Bug History** - Past issues and fixes are discoverable
4. **Progress Visibility** - Can see implementation status at any time
5. **Documentation Source** - Memory serves as source material for docs

## Best Practices

1. **Tag by Session** - Helps track chronological progress
2. **Tag by Status** - Use "in-progress", "completed", "blocked"
3. **Tag by Type** - "planning", "code", "bug-fix", "documentation"
4. **Semantic Queries** - Ask questions, don't just search keywords
5. **Cross-Reference** - Use code navigation tools alongside memory search
