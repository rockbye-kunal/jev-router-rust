# Contributing

Thanks for helping improve Jev Router.

## Before you start

- For a bug, open an issue with the request shape (no keys or private prompts), the response or error code, and the `x-request-id`.
- For a larger change, open an issue first so the approach can be agreed before you write code.

## Development

```sh
cargo build
cargo test --locked
scripts/local-integration.sh        # the same checks CI runs, without paid calls
```

Tests use local HTTP fixtures and never call OpenRouter. Only `scripts/local-integration.sh --live`, `jev-router smoke` and `jev-router acceptance` make billed calls, and they need your own key in `.env`.

## Pull requests

- Keep each pull request to one change, with a test that fails without it.
- Run `cargo fmt`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked` before pushing. CI also runs `cargo audit`.
- Update `CHANGELOG.md` under an "Unreleased" heading, and the README if behaviour or configuration changes.
- Never commit keys, `.env` files, databases or real user prompts.

## Releases

1. Move "Unreleased" entries in `CHANGELOG.md` to a new version section and bump `version` in `Cargo.toml`.
2. Merge to `main` with CI green.
3. Tag and push: `git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`. The release workflow builds the binaries and publishes the GitHub release with the changelog section.
