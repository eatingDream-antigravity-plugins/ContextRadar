# AGENTS.md

Guidance for AI coding agents working in `context-radar-mcp`. For agent usage instructions at runtime, see [skills/context-radar/SKILL.md](skills/context-radar/SKILL.md) (and [rules/AGENTS.md](rules/AGENTS.md) for the minimal `<400 B` Antigravity plugin rule). Humans: see [README.md](README.md).

## Architecture

- `src/engine/` — Read-only SQLite + WAL/JSONL telemetry (`telemetry.rs`), rate/pricing tables (`rates.rs`), hotspot/prefix-safe distillation (`pruner.rs`).
- `src/rpc/` — MCP stdio JSON-RPC server (`protocol.rs`), 5 public tools, SEP-1865 App-only tools, and scoped single-use handoff grants.
- `src/acp/` — Pure-protocol ACP thin client (`acp-proxy`, `acp-run`) forwarding `agy acp` stdio cleanly with zero telemetry/grant coupling.
- `src/guard/` — Fail-open `SessionStart` (`boot.rs`) and `PreToolUse` (`exec_gate.rs`) hooks.
- `panel/` — Zero-dependency dark-first SEP-1865 TypeScript UI (`pack.ts` -> `panel/dist/panel.html`, kept `<=64 KiB`).
- `plugin.json`, `mcp_config.json`, `hooks.json`, `rules/AGENTS.md`, `skills/context-radar/SKILL.md` — Official Antigravity plugin & portable Agent Skill bundle.

## Build, Test & Security Gates (run before every PR)

```sh
npm --prefix panel run typecheck && npm --prefix panel run build && npm --prefix panel test
cargo test --locked
./scripts/check_sanitized.sh
```

Release binary check (remap local paths out of binary):
```sh
RUSTFLAGS="--remap-path-prefix=$HOME=/build" cargo build --release --locked
```

## Invariants

- Open external Antigravity SQLite/state files strictly read-only (`SQLITE_OPEN_READ_ONLY`); write plugin state only under `--data-dir` with `0600`/`0700` permissions.
- Keep `rules/AGENTS.md` plain markdown `<= 400 B`; keep detailed tool workflows in `skills/context-radar/SKILL.md`.
- Keep `src/acp/` decoupled from `engine::telemetry` and `rpc::grants` (strict lower-triangular design).
- Never commit personal paths, tokens, emails, or real conversation IDs (`./scripts/check_sanitized.sh` must exit 0).
