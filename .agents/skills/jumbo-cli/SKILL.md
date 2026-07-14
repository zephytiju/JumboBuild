---
name: jumbo-cli
description: Verify version-matched Jumbo Build CLI commands, flags, defaults, aliases, and workflows. Use before running, recommending, documenting, reviewing, or changing any `jumbo` project or workspace command, including build, test, format, release, clean, workspace management, and shell completion.
---

# Jumbo CLI

Consult the executable help before relying on memory or prose documentation.

## Resolve the current command

Run the bundled helper with the command path under investigation:

```bash
.agents/skills/jumbo-cli/scripts/jumbo-help.sh
.agents/skills/jumbo-cli/scripts/jumbo-help.sh workspace remove
```

The helper uses the current JumboBuild source checkout when this skill belongs to that repository. Otherwise, it uses the installed `jumbo` binary. It prints the selected version followed by top-level or leaf help.

If the helper cannot find a source checkout or installed binary, report that limitation. Do not invent a command or flag. When available, use `docs/cli-reference.md` only as a navigation aid; live help from the selected version wins on conflict.

## Apply the reference

- Query the most specific affected command before proposing or executing it.
- Query each relevant parent and leaf when changing command structure or documentation.
- Preserve exact option names, value shapes, defaults, and command scope from help output.
- Distinguish project commands, which act on the registered project containing the current directory, from workspace commands.
- Treat destructive commands such as `workspace remove` with their documented confirmation behavior; never infer authorization from the existence of `--yes`.

## Maintain Jumbo CLI documentation

When changing JumboBuild's clap model or help text:

1. Update `src/cli/`, the command schema source of truth.
2. Run `cargo run --locked --example generate-cli-docs`.
3. Review `docs/cli-reference.md` for the intended command-tree change.
4. Run `cargo test` and the relevant live-help queries.
5. Ensure `git diff --exit-code -- docs/cli-reference.md` succeeds after regeneration is committed.

Do not edit the generated reference manually.
