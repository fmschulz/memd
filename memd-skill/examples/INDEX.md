# memd Usage Examples

Practical examples demonstrating common memd integration patterns.

## Available Examples

### 1. Session Context Tracking
**File:** `session_tracking.md`

**Use Case:** Maintain context across multiple coding sessions

**You'll Learn:**
- Recording work progress in memory
- Searching for context from previous sessions
- Tracking bugs and fixes chronologically
- Building documentation from memory
- Tagging strategies for session management

**Best For:** Long-running features spanning multiple days/weeks

---

### 2. Codebase Indexing
**File:** `codebase_indexing.md`

**Use Case:** Make entire codebases semantically searchable

**You'll Learn:**
- Batch indexing repository files
- Semantic code search across projects
- Code navigation with structural queries
- Finding similar code patterns
- Understanding architecture from memory

**Best For:** Large codebases, team onboarding, refactoring projects

---

### 3. Decision Tracking
**File:** `decision_tracking.md`

**Use Case:** Track architectural decisions and their rationale (ADRs)

**You'll Learn:**
- Recording Architecture Decision Records (ADRs)
- Retrieving decision rationale months later
- Cross-referencing related decisions
- Analyzing decision impact
- Tracking decision evolution over time

**Best For:** Team projects, long-term maintenance, onboarding, architecture reviews

---

## Quick Reference by Use Case

### For Individual Developers
- **Session work tracking** → `session_tracking.md`
- **Personal code search** → `codebase_indexing.md`
- **Learning from past work** → All examples

### For Teams
- **Architecture decisions** → `decision_tracking.md`
- **Codebase onboarding** → `codebase_indexing.md`
- **Knowledge preservation** → All examples

### For AI Agent Integration
- **Context continuity** → `session_tracking.md`
- **Code understanding** → `codebase_indexing.md`
- **Planning assistance** → `decision_tracking.md`

## Integration Patterns

### With Claude Code
All examples work seamlessly with Claude Code via MCP tools:
- `memory.add` for storing context
- `memory.search` for retrieval
- `code.find_*` for navigation

### With Codex CLI
Examples adapted for Codex CLI:
```bash
# Index codebase
codex -p "Index this repository using memd as tenant 'my-project'"

# Search memory
codex -p "Search memd for authentication implementation decisions"

# Code navigation
codex -p "Use memd to find all callers of function authenticate()"
```

### With GSD Workflow
Memory tracking complements GSD phases:
- `/gsd:plan-phase` → Search for related past work
- `/gsd:execute-phase` → Record implementation decisions
- `/gsd:verify-work` → Check completion against past context
- `/gsd:complete-milestone` → Archive decisions in memory

## Example Workflow Combinations

### 1. Feature Development with Memory
```
Day 1: Plan feature → Record plan in memory (session_tracking.md)
Day 2: Search past patterns → Implement (codebase_indexing.md)
Day 3: Record decisions → Document choices (decision_tracking.md)
Day 4: Review memory → Complete feature
```

### 2. Codebase Onboarding
```
Step 1: Index entire codebase (codebase_indexing.md)
Step 2: Search for architecture patterns
Step 3: Read ADRs from memory (decision_tracking.md)
Step 4: Explore code with semantic search
```

### 3. Long-Term Project Maintenance
```
Month 1: Record all architectural decisions (decision_tracking.md)
Month 2-6: Track implementation in sessions (session_tracking.md)
Month 6+: Search memory for "why" questions
Year 2: New developers use memory for onboarding
```

## Common Queries by Example

### Session Tracking Queries
```
"What was I working on last session?"
"Show me the bug I fixed yesterday"
"List all JWT-related work this sprint"
"What's the status of authentication feature?"
```

### Codebase Indexing Queries
```
"Find all database query functions"
"Show me similar API handler patterns"
"Where is User model referenced?"
"Explain the authentication flow"
```

### Decision Tracking Queries
```
"Why did we choose Kafka over RabbitMQ?"
"What decisions depend on Kubernetes?"
"Show me the evolution of our API design"
"Which ADR covers database strategy?"
```

## Tips for Getting Started

1. **Start Small** - Begin with one example (session_tracking.md recommended)
2. **Use Consistent Tenants** - One tenant per project
3. **Tag Everything** - Tags make retrieval much better
4. **Ask Questions** - Memory search works better with natural language
5. **Build Gradually** - Add more context over time
6. **Review Regularly** - Search memory weekly to reinforce patterns

## Next Steps

After reviewing examples:
1. Choose your primary use case
2. Follow the relevant example guide
3. Adapt queries to your project
4. Integrate with your AI agent workflow
5. See `../SKILL.md` for complete tool reference

## Combining Examples

Examples aren't mutually exclusive - use all three:

```markdown
# Morning routine
memory.search: "What did I work on yesterday?" (session tracking)
memory.search: "Show me similar error handling patterns" (codebase indexing)

# During implementation
memory.add: "Implementing user auth with JWT tokens" (session tracking)
code.find_definition: "JwtService" (codebase indexing)

# Making decisions
memory.search: "Past authentication architecture decisions" (decision tracking)
memory.add: "ADR-042: Use RS256 for JWT signing" (decision tracking)

# End of day
memory.add: "Completed JWT implementation, all tests passing" (session tracking)
```

This integrated approach creates a comprehensive memory system that supports all aspects of software development.
