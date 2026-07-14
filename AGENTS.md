# Repository instructions

## Jumbo CLI source of truth

- Use `$jumbo-cli` before running, recommending, documenting, reviewing, or changing a Jumbo command.
- Treat the clap model in `src/cli/` and help generated from the current source as the command schema source of truth.
- Treat `docs/cli-reference.md` as generated output; after CLI changes, run `cargo run --locked --example generate-cli-docs` and commit the result.
- Never guess Jumbo flags or reuse commands from stale prose documentation.
