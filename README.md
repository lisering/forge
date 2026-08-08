# Forge

> Autonomous software development agent that drives web-based AI chat via Chrome DevTools Protocol (CDP).

## Overview

Forge is a Rust-based autonomous software development agent. It controls Chrome browser via CDP (Chrome DevTools Protocol) to drive web-based AI chat services (chat.z.ai, chat.deepseek.com, kimi.moonshot.cn, tongyi.aliyun.com, claude.ai) for multi-stage autonomous software development:

```
Mission → AI decompose → Task-by-task code generation → Compile/test → Error feedback & fix → Repeat until complete → ZIP package
```

## Features

- **Multi-site support**: Z.ai, DeepSeek, Kimi, Tongyi, Claude.ai with automatic failover
- **Autonomous clarification**: Heuristic + local LLM (Ollama) hybrid clarification
- **Slash commands**: AI can emit `/compact`, `/skip`, `/refocus`, `/retry`, `/escalate`
- **Multi-language**: Rust (cargo), Python (pytest), Go (go), Node (npm)
- **Parallel execution**: TaskGraph DAG for parallel task execution
- **Intelligent error diagnosis**: Root cause analysis + classification + historical learning
- **Context handoff**: Auto-start new conversation with context when thread gets too long
- **Chrome auto-recovery**: Connection monitoring + exponential backoff reconnect
- **Proactive rate-limit detection**: Detect rate-limit text in AI responses and switch sites
- **Structured dev tracing**: Detailed trace of every AI interaction in `.forge/devtrace.jsonl`
- **Version management**: Pre-write snapshots + known-good rollback

## Quick Start

### Prerequisites

- Rust 1.70+ (2021 edition)
- Chrome browser
- Access to at least one supported AI chat site (logged in)

### Build

```bash
cargo build --release
```

### Launch Chrome with debugging

```bash
google-chrome \
  --remote-debugging-port=9222 \
  --user-data-dir=/Users/$USER/.forge-chrome \
  https://chat.deepseek.com \
  https://chat.z.ai
```

Log in to the AI chat sites in the opened Chrome window.

### Usage

```bash
# List detected chat tabs
cargo run -- list

# Check site health
cargo run -- health

# Send a single message
cargo run -- ask "5+5=?" --timeout 120 --tab 0

# Full development flow
cargo run -- run "Create a Rust CLI calculator" --max-rounds 2 --timeout 300 --phase2-timeout 300 --tab 0

# 24h stress test
./scripts/stress_test.sh "Create a Rust CLI calculator"
```

### Quality Gate

```bash
./scripts/quality_gate.sh  # fmt + clippy + test + coverage + audit
```

## Architecture

```
src/
├── main.rs              # CLI entry point
├── cdp.rs               # CDP WebSocket connection
├── chat.rs              # Chat page operations (send/wait/streaming)
├── browser.rs           # Browser manager (tab discovery)
├── orchestrator.rs      # Core orchestration engine (DIP architecture)
├── traits.rs            # DIP trait definitions
├── workspace.rs         # Workspace management (files/snapshots)
├── extract.rs           # Code extraction from AI responses
├── package.rs           # ZIP packaging
├── memory.rs            # Development memory
├── testrunner.rs        # Test runner (cargo check/test)
├── language.rs          # Multi-language adapters
├── clarify.rs          # Heuristic clarification checker
├── llm_clarify.rs       # LLM-enhanced clarification
├── failover_chat.rs     # Multi-site failover ChatClient wrapper
├── site_health.rs       # Site health checker
├── prompt_builder.rs    # System prompt builder
├── task_graph.rs        # Task dependency graph (DAG)
├── error_diagnosis.rs   # Intelligent error diagnosis
├── context_handoff.rs   # Context handoff
├── steer_reminder.rs    # Steer reminder
├── loop_detector.rs     # Loop termination detection
├── dev_trace.rs         # Structured dev tracing
├── slash_command.rs     # AI slash commands
├── connection_monitor.rs # Chrome connection monitor
├── auto_recovery.rs     # Auto recovery mechanism
└── interaction.rs       # Human interaction (Auto/Cli/Mock)
```

## Testing

```bash
# Run all tests
cargo test

# Run with nextest (faster)
cargo nextest run --workspace

# Coverage report
cargo llvm-cov --workspace

# Property-based tests
cargo test -- proptest
```

## Constraints

This project follows the [System Constraints](constraints/SYSTEM_CONSTRAINTS.md) (v2.1, 16 categories, 798 lines), including:

- Cutting-edge technology requirements
- SOLID architecture principles
- Spec-Driven Development
- TDD (Test-Driven Development)
- Code quality standards
- Security best practices
- AI code review & verification
- Vibe Coding automated testing system

## License

MIT
