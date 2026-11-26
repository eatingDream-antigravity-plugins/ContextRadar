# Changelog

All notable changes to this project will be documented in this file.

## [1.4.0](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.3.4...v1.4.0) (2026-11-26)


### Features

* **ai:** add llms.txt, root AGENTS.md, and cross-client AI integration docs ([#25](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/25)) ([00a55e1](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/00a55e1dfb1eb300eefa0408d3f4899c9c4274b8))

## [1.3.4](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.3.3...v1.3.4) (2026-11-26)


### Bug Fixes

* **acp:** make acp-proxy and acp-run a pure-protocol thin client ([#23](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/23)) ([42a93c6](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/42a93c6cb3d6040ff84bdf7b809664062180c558))

## [1.3.3](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.3.2...v1.3.3) (2026-11-26)


### Bug Fixes

* **arch:** decouple telemetry tiers, lock grant store, and unify handoff contracts ([#21](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/21)) ([6b1efdc](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/6b1efdc44c8e31dae202e3f60e40e3c3231c6045))

## [1.3.2](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.3.1...v1.3.2) (2026-11-24)


### Bug Fixes

* **panel:** dock hero subagents in focus card and fix layout, tabs, and reasoning stream TPS ([#19](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/19)) ([afc596c](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/afc596cf49c82417e24bb349f096fb4fbbd424e6))

## [1.3.1](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.3.0...v1.3.1) (2026-11-24)


### Code Refactoring

* trim redundant tests and tighten documentation ([#17](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/17)) ([646accb](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/646accbbff9bee8ebbd7135dceababe7bdebca1b))

## [1.3.0](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.2.0...v1.3.0) (2026-11-24)


### Features

* add 4-state live execution telemetry, thinking/response split, and loop rollup ([#16](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/16)) ([a8a94e4](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/a8a94e452122561ddb370d920c3113ba03ca4adb))


### Bug Fixes

* **docs:** serve release badge from first-party assets/release-badge.svg ([#14](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/14)) ([5697555](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/569755529e90a9a25aac6f8916e155af3c6504d1))

## [1.2.0](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.1.0...v1.2.0) (2026-11-24)


### Features

* add official ACP v1 auto-compact proxy/runner, zero-lag refresh, and fix MCP tool limit & README badge ([#10](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/10)) ([7ba3935](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/7ba39355ffa012fe61cadf2e56f729ead4ea0a2b))


### Bug Fixes

* **docs:** use shields static/v1 query URL so release-please preserves badge color ([#13](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/13)) ([e503a15](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/e503a1512ae44dfc96d527289ae10e25a48544f9))
* **panel:** render background polls, refresh on pane reveal, and support guarded App compact dispatch ([#12](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/12)) ([22ab508](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/22ab508ab886e02fd48b89e8eb4093a7e655ef3c))

## [1.1.0](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/compare/v1.0.0...v1.1.0) (2026-11-24)


### Features

* add Gemini 4 180s prefill TTL, mid-loop sub-reasons, and Carbon concurrency warning ([#9](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/9)) ([864307f](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/864307fa0e4a0fea5981709767c642eef902cf72))
* complete Antigravity model pricing, scoped grants, bilingual UI, and safety docs ([#8](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/issues/8)) ([95ce974](https://github.com/eatingDream-antigravity-plugins/context-radar-mcp/commit/95ce974d3f369223bad5a99b7b2eb8351526700a))


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
