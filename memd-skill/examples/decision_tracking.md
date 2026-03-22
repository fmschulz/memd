# Cross-Agent Experiment and Decision Tracking

This example shows how several agents can collaborate on the same tenant and later recover what worked, what failed, and why.

## Scenario

Tenant: `microservices-platform`

Question: choose the right eventing stack and validate operational trade-offs.

The important design principle is that decisions should not be stored as isolated prose only. They should be stored as task artifacts with:

- motivation
- alternatives
- runs
- evidence
- worked/failed summaries
- remaining uncertainty

## Agent A Starts the Evaluation

```json
{
  "name": "task.start",
  "arguments": {
    "tenant_id": "microservices-platform",
    "project_id": "platform-architecture",
    "goal": "Select the event bus for the microservices platform",
    "motivation": "The platform needs durable event propagation, replay, and operational reliability",
    "hypothesis": "Kafka will satisfy throughput and replay requirements better than RabbitMQ",
    "scientific_question": "Which event bus best matches replay, throughput, and operability constraints?",
    "dataset_refs": [
      {"name": "platform_requirements", "version": "adr-input-v2"}
    ],
    "expected_outputs": [
      "decision summary",
      "operational trade-off list",
      "follow-up migration plan"
    ]
  }
}
```

## Agent A Logs an Evaluation Run

```json
{
  "name": "task.run_start",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>",
    "project_id": "platform-architecture",
    "tool_name": "benchmark-runner",
    "tool_version": "1.2.0",
    "command": "benchmark-runner eventbus --candidates kafka rabbitmq",
    "why_chosen": "Need comparable throughput and replay measurements under the same workload",
    "parameters": {
      "messages_per_second": 100000,
      "replay_test": true
    },
    "inputs": [
      "platform_requirements",
      "benchmark_scenarios.yaml"
    ]
  }
}
```

```json
{
  "name": "task.run_finish",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>",
    "project_id": "platform-architecture",
    "status": "completed",
    "tool_name": "benchmark-runner",
    "outputs": [
      "kafka_throughput=112000",
      "rabbitmq_throughput=38000",
      "replay_supported=true only for kafka path"
    ],
    "metrics": {
      "kafka_mps": 112000,
      "rabbitmq_mps": 38000
    },
    "notes": "Kafka satisfied throughput and replay goals; RabbitMQ remained easier operationally",
    "validation": [
      "Benchmark scenario reproduced twice with similar results"
    ]
  }
}
```

## Agent A Records Evidence

```json
{
  "name": "task.add_evidence",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>",
    "project_id": "platform-architecture",
    "summary": "Kafka exceeded throughput requirements and uniquely satisfied replay requirements",
    "evidence_kind": "benchmark_result",
    "supports_claim": true,
    "metrics": {
      "kafka_mps": 112000,
      "rabbitmq_mps": 38000,
      "replay_supported": true
    }
  }
}
```

## Agent B Searches What Already Happened

Agent B should not restart the evaluation from scratch.

```json
{
  "name": "task.search",
  "arguments": {
    "tenant_id": "microservices-platform",
    "query": "event bus replay throughput evidence",
    "k": 10,
    "filters": {
      "project_id": "platform-architecture",
      "tool_name": "benchmark-runner"
    }
  }
}
```

Agent B can also inspect the canonical task history:

```json
{
  "name": "task.get",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>"
  }
}
```

## Agent B Adds Failure Knowledge

Suppose Agent B tests an operational prototype and finds a problem.

```json
{
  "name": "task.progress",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>",
    "project_id": "platform-architecture",
    "summary": "Operational prototype exposed a painful bootstrap path for local development",
    "blockers": [
      "Kafka cluster setup is slower for new contributors"
    ],
    "failed_attempts": [
      "A docker-compose setup with three brokers was too heavy for local onboarding"
    ],
    "next_step": "Test a lighter local single-broker workflow and document the trade-off"
  }
}
```

That failure is now discoverable by later agents. It does not disappear into chat history.

## Agent C Finishes the Task

```json
{
  "name": "task.finish",
  "arguments": {
    "tenant_id": "microservices-platform",
    "task_id": "<task_id>",
    "project_id": "platform-architecture",
    "what_worked": [
      "Kafka satisfied replay and throughput requirements",
      "Benchmark evidence was reproducible",
      "A lightweight local single-broker setup reduced onboarding cost"
    ],
    "what_failed": [
      "RabbitMQ did not satisfy replay requirements",
      "The first multi-broker local prototype was too operationally heavy"
    ],
    "validation": [
      "Benchmark outputs matched requirements",
      "Operational prototype review confirmed local workflow viability"
    ],
    "uncertainty": [
      "Production observability costs still need forecasting"
    ],
    "followups": [
      "Write the ADR from the canonical task history",
      "Publish local setup guidance"
    ],
    "confidence": 0.84
  }
}
```

## Why This Is Better Than Free-Form Notes

Later agents can answer:

- Why did we choose Kafka?
- What evidence supported that choice?
- What failed during prototyping?
- Which parameters were used during evaluation?
- What uncertainty remains?

The knowledge is no longer trapped in one agent’s memory or one chat transcript. It is normalized and searchable across agents in the same tenant.
