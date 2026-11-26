# Contributing

Thanks for contributing to [Context Radar MCP](https://github.com/eatingDream-antigravity-plugins/ContextRadar)!

## Workflow

- Use short-lived branches from `main` (`feat/`, `fix/`, `docs/`, `refactor/`, `test/`, `chore/`, `ci/`) and submit Pull Requests with [Conventional Commits](https://www.conventionalcommits.org/) titles.
- Configure a GitHub noreply commit email (`<id>+<login>@users.noreply.github.com`); never commit secrets, tokens, private keys, personal hostnames/paths, or real credentials.
- Contributions are licensed under the [MIT License](LICENSE).

## Local checks before pushing

```bash
node panel/pack.mjs
node --test panel/panel.test.mjs
cargo test --locked
bash scripts/check_sanitized.sh
```

Release binaries should be built with `RUSTFLAGS="--remap-path-prefix=$HOME=/build" cargo build --release --locked` so local paths are not embedded.
