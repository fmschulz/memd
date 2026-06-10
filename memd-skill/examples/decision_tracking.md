# Cross-Agent Experiment and Decision Tracking

This example shows how agents can preserve evidence and decisions through the
CLI.

Tenant: `microservices-platform`
Project: `platform-architecture`
Task tag: `event-bus`

## Agent A: Search Before Starting

```bash
memd agent-context \
  --tenant-id microservices-platform \
  --project-id platform-architecture \
  --query "event bus replay throughput prior decisions" \
  --k 2 \
  --token-budget 700 \
  --format markdown \
  --output .memd/context.md \
  --log-dir .memd/search-logs
```

## Agent A: Record the Benchmark Run

```bash
memd add \
  --tenant-id microservices-platform \
  --project-id platform-architecture \
  --chunk-type trace \
  --tags kind:run,task:event-bus,tool:benchmark-runner,status:completed \
  --text "benchmark-runner eventbus --candidates kafka rabbitmq: kafka_throughput=112000, rabbitmq_throughput=38000, replay_supported=true only for kafka."
```

## Agent A: Record Evidence

```bash
memd add \
  --tenant-id microservices-platform \
  --project-id platform-architecture \
  --chunk-type research \
  --tags kind:evidence,task:event-bus,supports:kafka \
  --text "Kafka exceeded throughput requirements and uniquely satisfied replay requirements. RabbitMQ remained operationally simpler but missed replay and throughput targets."
```

## Agent A: Record the Decision

```bash
memd add \
  --tenant-id microservices-platform \
  --project-id platform-architecture \
  --chunk-type decision \
  --tags kind:decision,task:event-bus \
  --text "Choose Kafka for the event bus because replay and throughput are hard requirements. Follow-up: write migration and operator runbook."
```

## Agent B: Recover the Decision

```bash
memd search \
  --tenant-id microservices-platform \
  --project-id platform-architecture \
  --query "event bus replay throughput decision" \
  --mode find_decisions \
  --compact \
  --token-budget 2000 \
  --format markdown
```
