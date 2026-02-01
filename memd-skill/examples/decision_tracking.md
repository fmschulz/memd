# Decision Tracking Example

Track architectural decisions and their rationale using memd for future reference.

## Scenario

You're building a microservices platform and need to track why certain technology and architecture decisions were made.

## Architecture Decision Records (ADR) in Memory

### Decision 1: Message Queue Selection

```markdown
User: "We need to decide between RabbitMQ and Kafka for our event bus"

Agent: Let me search for any previous messaging decisions...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "message queue event streaming decisions",
  "k": 5
}

No results found. This is a new decision.

User: "Go with Kafka. We need event replay and high throughput."

Agent: Recording this architectural decision.

<memory.add>
{
  "tenant_id": "microservices-platform",
  "text": "ADR-001: Event Bus Technology Selection. Decision: Use Apache Kafka instead of RabbitMQ. Rationale: (1) Need event replay for audit trails, (2) High throughput requirements (100K+ events/sec), (3) Event sourcing pattern requires durable log, (4) Team has Kafka experience. Trade-offs: More complex ops than RabbitMQ, higher resource usage. Alternatives considered: RabbitMQ (simpler but no replay), AWS Kinesis (vendor lock-in), Redis Streams (not production-ready).",
  "chunk_type": "decision",
  "tags": ["adr", "kafka", "messaging", "architecture", "adr-001"]
}
```

### Decision 2: Service Mesh

```markdown
User: "Should we use Istio or stick with direct HTTP calls?"

Agent: Let me check related architectural decisions...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "service communication architecture patterns",
  "k": 10
}

Agent: No previous service mesh decisions found.

User: "Use Istio. We need observability and traffic management."

<memory.add>
{
  "tenant_id": "microservices-platform",
  "text": "ADR-002: Service Mesh Adoption. Decision: Implement Istio service mesh. Rationale: (1) Need distributed tracing across 15+ services, (2) Circuit breaker and retry logic critical for reliability, (3) mTLS for zero-trust security, (4) Traffic splitting for canary deployments. Trade-offs: Adds complexity and latency (~5ms per hop), steep learning curve. Alternatives: Direct HTTP (too manual), Linkerd (less feature-complete), AWS App Mesh (vendor lock-in). Requires Kubernetes 1.20+.",
  "chunk_type": "decision",
  "tags": ["adr", "istio", "service-mesh", "kubernetes", "adr-002"]
}
```

### Decision 3: Database Strategy

```markdown
User: "How should we handle databases in microservices?"

Agent: Searching for related architecture decisions...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "microservices database data architecture",
  "k": 10
}

Found: ADR-001 (Kafka for events), ADR-002 (Istio)...

User: "Each service gets its own database. Use Postgres for most, Redis for caching."

<memory.add>
{
  "tenant_id": "microservices-platform",
  "text": "ADR-003: Database Per Service Pattern. Decision: Each microservice owns its database schema. Use PostgreSQL as default RDBMS, Redis for caching/sessions, MongoDB for document storage (logs service only). Rationale: (1) Service autonomy and independent deployment, (2) No shared database coupling, (3) Polyglot persistence where beneficial. Trade-offs: Data consistency challenges (eventual consistency via events), operational complexity (15+ databases), no joins across services. Implementation: Cross-service queries via API calls or materialized views populated from Kafka events (ref: ADR-001). Alternatives: Shared database (tight coupling), API-only (too chatty).",
  "chunk_type": "decision",
  "tags": ["adr", "database", "postgres", "redis", "microservices", "adr-003"]
}
```

## Retrieving Decisions Later

### 3 Months Later: New Developer Onboarding

```markdown
New Developer: "Why are we using Kafka? Seems like overkill."

Agent: Let me find the decision rationale...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "why kafka event bus decision",
  "k": 3
}

Result: "ADR-001: Event Bus Technology Selection. Decision: Use Apache Kafka instead of RabbitMQ. Rationale: (1) Need event replay for audit trails, (2) High throughput requirements (100K+ events/sec)..."

Agent: We chose Kafka because:
1. Event replay is critical for our audit requirements
2. We process 100K+ events/second (RabbitMQ wouldn't scale)
3. Event sourcing pattern requires durable log
4. Team already had Kafka experience

The decision was made in ADR-001 and is still valid.
```

### 6 Months Later: Performance Review

```markdown
User: "Istio is adding too much latency. Should we remove it?"

Agent: Let me check why we adopted Istio...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "istio service mesh decision rationale",
  "k": 5
}

Result: "ADR-002: Service Mesh Adoption. Decision: Implement Istio service mesh. Rationale: (1) Need distributed tracing across 15+ services, (2) Circuit breaker and retry logic critical for reliability, (3) mTLS for zero-trust security, (4) Traffic splitting for canary deployments. Trade-offs: Adds complexity and latency (~5ms per hop)..."

Agent: According to ADR-002, we expected ~5ms latency per hop as a known trade-off. The benefits were:
- Distributed tracing (critical for debugging)
- Circuit breakers (preventing cascading failures)
- mTLS security
- Canary deployments

Question: Are we still using these features? If the 5ms latency is unacceptable, we could:
1. Remove Istio and implement circuit breakers in-app
2. Use Linkerd (lower latency)
3. Optimize Istio config

Let me search for any related performance requirements...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "performance latency requirements SLA",
  "k": 10
}
```

### Cross-Referencing Decisions

```markdown
User: "We're adding a new notifications service. What database should it use?"

Agent: Checking database architecture decisions...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "database strategy microservices",
  "k": 5
}

Result: "ADR-003: Database Per Service Pattern. Decision: Each microservice owns its database schema. Use PostgreSQL as default RDBMS..."

Agent: According to ADR-003, each service gets its own database. Default is PostgreSQL unless there's a specific reason for an alternative.

For notifications service:
- PostgreSQL would work (structured data, ACID transactions)
- Consider Redis if you need pub/sub for real-time delivery
- MongoDB if notification content is highly variable

The decision also mentions cross-service queries should use API calls or Kafka materialized views (ref: ADR-001).

Do you need real-time delivery or can notifications be eventual consistency?
```

## Decision Impact Analysis

### Finding Affected Decisions

```markdown
User: "We're considering migrating from Kubernetes to AWS ECS. What would be impacted?"

Agent: Searching for Kubernetes-dependent decisions...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "kubernetes infrastructure dependencies",
  "k": 20
}

Results:
1. ADR-002: "Requires Kubernetes 1.20+. (Istio service mesh)"
2. ADR-007: "Use Kubernetes StatefulSets for Kafka brokers"
3. ADR-012: "Helm charts for service deployment on K8s"

Agent: Found 3 architectural decisions that depend on Kubernetes:

**Critical Impact:**
- ADR-002 (Istio): Requires K8s. ECS alternative would be AWS App Mesh (less mature)

**Medium Impact:**
- ADR-007 (Kafka): StatefulSets manage persistence. ECS equivalent is ECS Service with EBS volumes
- ADR-012 (Deployment): Helm charts need migration to ECS task definitions

This is a major architectural change affecting service mesh, deployment, and stateful workloads. Estimated effort: 3-6 months for full migration.
```

## Decision Evolution Tracking

### Superseding Decisions

```markdown
User: "We're moving away from Istio. It's too complex for our team."

Agent: Recording decision update...

<memory.add>
{
  "tenant_id": "microservices-platform",
  "text": "ADR-015: Deprecate Istio Service Mesh. Decision: Remove Istio and use application-level circuit breakers (resilience4j). Rationale: (1) Team lacks Istio expertise, operational burden too high, (2) Latency impact (8ms actual vs 5ms expected), (3) Distributed tracing moved to OpenTelemetry in ADR-014, (4) mTLS now handled by AWS NLB. Trade-offs: Loss of centralized traffic management, circuit breakers now in-app responsibility, no canary deployments (use blue/green instead). Supersedes: ADR-002 (Istio adoption). Migration plan: Phase out over 3 sprints, service-by-service.",
  "chunk_type": "decision",
  "tags": ["adr", "istio", "deprecation", "resilience4j", "adr-015", "supersedes-adr-002"]
}

Agent: ADR-015 now supersedes ADR-002. The original Istio decision is deprecated but preserved in memory for historical context.
```

### Querying Decision Timeline

```markdown
User: "Show me the evolution of our service communication architecture"

Agent: Searching chronologically...

<memory.search>
{
  "tenant_id": "microservices-platform",
  "query": "service communication architecture mesh http",
  "k": 20
}

Results (chronological):
1. ADR-002 (2025-06): "Adopt Istio service mesh"
2. ADR-008 (2025-09): "Add API gateway (Kong) for external traffic"
3. ADR-014 (2025-11): "Migrate to OpenTelemetry for observability"
4. ADR-015 (2026-01): "Deprecate Istio, use resilience4j"

Agent: Service communication architecture evolved:

**Phase 1 (June 2025):** Direct HTTP → Istio mesh
- Reason: Needed observability and traffic management

**Phase 2 (Sep 2025):** Added Kong API gateway
- Reason: External traffic management, rate limiting

**Phase 3 (Nov 2025):** OpenTelemetry replaces Istio tracing
- Reason: Vendor-neutral, better tooling

**Phase 4 (Jan 2026):** Removed Istio entirely
- Reason: Operational complexity, team expertise

Current state: Kong + OpenTelemetry + resilience4j in-app
```

## Benefits

1. **Institutional Memory** - Decisions preserved even as team members change
2. **Rationale Tracking** - Future developers understand *why* not just *what*
3. **Impact Analysis** - Find all decisions affected by a proposed change
4. **Decision Evolution** - Track how architecture evolved over time
5. **Onboarding** - New developers quickly understand architecture philosophy

## Best Practices

1. **Consistent Format** - Use ADR numbering (ADR-001, ADR-002...)
2. **Rich Tags** - Include technology names, categories, ADR numbers
3. **Link Related Decisions** - Reference other ADRs explicitly
4. **Include Trade-offs** - Document what you're giving up
5. **List Alternatives** - Show what else was considered
6. **Supersede, Don't Delete** - Preserve historical decisions with supersedes tags
7. **Semantic Queries** - Ask "why did we choose X" not just "X decision"

## Integration with Planning Skills

```bash
# With /gsd:plan-phase
memory.search: "architecture decisions related to API design"
# Use findings to inform implementation plan

# With /plan
memory.search: "past decisions about error handling patterns"
# Ensure new features align with established patterns

# With /codex-plan
# Codex can search memory for architectural context
# before generating implementation plan
```

This approach transforms architectural knowledge from tribal knowledge into searchable, persistent institutional memory.
