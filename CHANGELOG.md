# Changelog

All notable changes to this project will be documented in this file.

## [1.0.1](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.0.0...v1.0.1) (2026-11-23)


### Documentation

* **readme:** rewrite bilingual READMEs for human readability and usage focus ([#4](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/4)) ([59e1298](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/59e1298e01b7356a4c10c9a9f51a5fb7a895649c))

## [1.0.0](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/releases/tag/v1.0.0) (2026-11-24)

### Features

- Native Rust context telemetry daemon (`context-radar-mcp daemon`) with background pre-warm and `< 0.5 ms` Stale-While-Revalidate (SWR) boot snapshots.
- Strict read-only Antigravity SQLite inspection (`SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only = ON`) with isolated writable state under `${PLUGIN_DATA}`.
- 3-way cold-cache attribution (`turn_start_cold`, `mid_loop_cold`, `post_compact_cold`), per-turn TTFT / throughput / cost analytics, and zero-copy early-exit protobuf wire decoding.
- Prefix-safe 256 KiB reverse `CHECKPOINT` hotspot scanner and slice distiller (`context_radar_hotspots`) writing `< 600` token digests to session `scratch/`.
- Model × reasoning-effort budget tiers, economic break-even gate, single-use replay-protected handoff grants, 1-click `/compact`, and official `target:new` relay cards (`context_radar_prepare_handoff`, `context_radar_commit_handoff`).
- Editorial-Warm dark-first `< 45 KiB` SEP-1865 MCP App UI (`ui://context-radar-mcp/panel`) with transparent-background host CSS guards, single-mount in-place DOM patching, and RTT-adaptive weak-network polling.
- Native Rust `SessionStart` (`guard boot`) and `PreToolUse(run_command)` (`guard exec`) execution guards.
- Cross-platform GitHub Releases workflow, Release Please automation, bilingual EN/ZH documentation, and POSIX `scripts/install.sh` installer.
