### Commit Convention

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>
```

- **type**: Use `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, etc.
- **scope**: Use the affected module/component (e.g., `status`, `apply`, `config`). Use `docs` for documentation-only changes.
- **description**: Short, lowercase imperative mood (no period, no trailing newline)

Examples:
- `feat(status): add diff view for changed files`
- `fix(apply): handle permission errors gracefully`
- `docs(readme): update installation instructions`

Commit subject line must be ≤72 characters.

### Verification

Before submitting changes, run clippy to verify no warnings:

```bash
cargo clippy -- -W clippy::all -W clippy::perf -W clippy::pedantic
```
