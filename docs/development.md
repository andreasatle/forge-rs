# Development

## Git hooks

Enable hooks after cloning by running the setup script once:

```bash
./scripts/setup-hooks.sh
```

This configures Git to use `.githooks/` as the hooks directory and makes the scripts executable.

### Why `.githooks/` and not `.git/hooks/`

`.git/hooks/` is local to your clone and not tracked by Git — hooks placed there are not shared with other developers. `.githooks/` is committed to the repository, so every clone gets the same hooks after running the setup script.

### What the hooks do

**pre-commit** — runs before every commit:

- `cargo fmt -- --check` — fails if code is not formatted
- `cargo test` — fails if any test fails
- `cargo clippy -- -D warnings` — fails if there are any clippy warnings

The hook validates only; it does not auto-format or modify files.

**commit-msg** — runs after you type a commit message:

- Rejects empty commit messages (ignoring comment lines).

### New clone setup

```bash
git clone <repo>
cd forge-rs
./scripts/setup-hooks.sh
```

The setup script only needs to be run once per clone.
