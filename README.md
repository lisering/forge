# Forge

> Autonomous software development agent that drives web-based AI chat via Chrome DevTools Protocol (CDP).

[![CI](https://github.com/lisering/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/lisering/forge/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust Edition 2021](https://img.shields.io/badge/Rust-2021-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-blue.svg)](#)

## Overview

Forge is a Rust-based autonomous software development agent. It controls Chrome browser via CDP to drive web-based AI chat services (chat.deepseek.com, chat.z.ai, kimi.moonshot.cn, tongyi.aliyun.com, claude.ai) for multi-stage autonomous software development:

```
Mission → AI decompose → Task-by-task code generation → Compile/test
  → Error feedback & fix → Repeat until complete → ZIP package
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

- [Rust](https://www.rust-lang.org/tools/install) 1.70+ (2021 edition)
- [Chrome browser](https://www.google.com/chrome/)
- Access to at least one supported AI chat site (logged in)

### Build

```bash
git clone https://github.com/lisering/forge.git
cd forge
cargo build --release
```

### Launch Chrome with Debugging

```bash
google-chrome \
  --remote-debugging-port=9222 \
  --user-data-dir=$HOME/.forge-chrome \
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
cargo run -- run "Create a Rust CLI calculator" \
  --max-rounds 2 --timeout 300 --phase2-timeout 300 --tab 0

# 24h stress test
./scripts/stress_test.sh "Create a Rust CLI calculator"
```

## Architecture

Forge follows SOLID principles, especially **DIP (Dependency Inversion)** — the
Orchestrator depends on trait abstractions, enabling core logic testing without Chrome.

For the full architecture document, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

```
src/
├── main.rs                # CLI entry point
├── lib.rs                 # Crate root (module declarations + re-exports)
├── orchestrator.rs        # Core orchestration engine (DIP architecture)
├── traits.rs              # DIP trait definitions (ChatClient, TestRunner, etc.)
├── cdp.rs                 # CDP WebSocket low-level connection
├── chat.rs                # Chat page operations (send/wait/streaming)
├── browser.rs             # Browser manager (tab discovery, site detection)
├── workspace.rs           # Workspace management (files/snapshots/rollback)
├── extract.rs             # Code extraction from AI responses
├── package.rs             # ZIP packaging
├── memory.rs              # Development memory (conversation history)
├── testrunner.rs          # Test runner (cargo build/test)
├── language.rs            # Multi-language adapters (Rust/Python/Go/Node)
├── clarify.rs             # Heuristic autonomous clarification
├── llm_clarify.rs         # LLM-enhanced clarification (Ollama)
├── failover_chat.rs       # Multi-site automatic failover
├── site_health.rs         # Website health checking
├── connection_monitor.rs  # Chrome connection monitoring
├── auto_recovery.rs       # Auto-recovery (exponential backoff)
├── task_graph.rs          # Task dependency graph (DAG parallelism)
├── error_diagnosis.rs     # Intelligent error diagnosis
├── context_handoff.rs     # Context handoff for long conversations
├── steer_reminder.rs      # Goal steer reminder injection
├── loop_detector.rs       # Loop termination detection
├── dev_trace.rs           # Structured development tracing (JSONL)
├── slash_command.rs       # AI self-commands (/compact /skip /refocus)
├── prompt_builder.rs      # System prompt builder
└── interaction.rs         # Human interaction (Auto/Cli/Mock)
```

For the complete module and symbol index, see [docs/MODULE_INDEX.md](docs/MODULE_INDEX.md).

## Documentation

### Generate API Docs

```bash
# Generate and open HTML documentation
cargo doc --open

# Check for documentation warnings (must be zero)
cargo doc --no-deps
```

### Documentation Files

| File | Description |
|------|-------------|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture overview and design principles |
| [docs/MODULE_INDEX.md](docs/MODULE_INDEX.md) | Complete module map, trait hierarchy, and symbol index |
| [docs/TRAITS.md](docs/TRAITS.md) | DIP trait definitions and dependency injection patterns |
| [docs/adr/](docs/adr/) | Architecture Decision Records (ADRs) |

## Testing

```bash
# Run all tests
cargo test

# Run with nextest (faster, recommended)
cargo nextest run --workspace

# Coverage report
cargo llvm-cov --workspace

# Property-based tests
cargo test -- proptest
```

### Test Pyramid (70:20:10)

| Type | Proportion | Description |
|------|-----------|-------------|
| Unit | 70% | Single function/module, no external dependencies, uses Mock |
| Integration | 20% | Module collaboration, limited external dependencies |
| E2E | 10% | End-to-end flow, only critical paths |

### Quality Gate

```bash
./scripts/quality_gate.sh  # fmt + clippy + test + audit
```

## Supported AI Chat Sites

| Site | URL | Role |
|------|-----|------|
| DeepSeek | `chat.deepseek.com` | Primary |
| Z.ai | `chat.z.ai` | Backup |
| Kimi | `kimi.moonshot.cn` | Fallback |
| Tongyi | `tongyi.aliyun.com` | Fallback |
| Claude | `claude.ai` | Fallback |

## Scripts

| Script | Description |
|--------|-------------|
| `scripts/sync_github.sh --push` | Sync source code to GitHub (run after each session) |
| `scripts/quality_gate.sh` | Quality gate: fmt + clippy + test + audit |
| `scripts/stress_test.sh` | 24h stress test for reliability validation |

## Constraints

This project follows the System Constraints (v2.1, 16 categories, 798 lines), including:

- Cutting-edge technology requirements
- SOLID architecture principles
- Spec-Driven Development
- TDD (Test-Driven Development)
- Code quality standards (Rust API Guidelines)
- Security best practices
- AI code review & verification (6 rules)
- Structured logging & observability
- LLM application security
- Error handling & resilient design
- Vibe Coding automated testing system

## License

[MIT](LICENSE)
