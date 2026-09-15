# Security

These decisions are intentional. Do not "fix" them with a half-measure.

## Path denylist (non-negotiable by default)

Writes/reads whose **resolved final path** matches `.env` / `.env.*`, `.git`,
`.ssh`, or `credentials` are blocked. This check is independent of the
generic "sensitive tool" confirmation:

- `--yes` / UI checkbox for sensitive tools does **not** punch through the denylist.
- Skipping the denylist requires a second, explicit confirmation
  (`--allow-denied-paths`, or typing `ALLOW DENIED PATH` in the REPL).
- The sandbox still re-checks the resolved path and the override must match
  that path, so approving `.env` does not authorize a different target.

`write_file` and `Sandbox::rename` resolve the parent with
`std::fs::canonicalize`, join the file name, and run the denylist on that
final path (and on a full canonicalize if the target already exists). So
`write("config")` then `rename(".env")` is blocked, as is `innocent → .env`.

### Accepted gap: shell rename

A `run_command` can still `echo SECRET > tmp && mv tmp .env`. Filename policy
on the write/rename **API** cannot see mutations the shell performs. Closing
that fully means intercepting every filesystem mutation inside the sandbox
(Landlock is an allowlist of trees, not a filename denylist). Documented,
not hidden.

## Landlock `SCOPE_SIGNAL` (Linux < 6.12)

Landlock ABI 6 (Linux 6.12+) can scope signals so a sandboxed process cannot
`kill -9` other processes of the same user. On older kernels this is
**open**. Forger logs a runtime warning and continues. It must never fail
quietly and pretend the hole is closed.

## `forger serve` has no authentication

The HTTP/SSE surface binds **loopback only**. That is acceptable for
single-user local use. Binding it to the network is local RCE (the agent
runs commands as you).

We will **not** add a decorative bearer token. Authentication is either a
real session (token/cookie) or it stays unimplemented. The CLI refuses
non-loopback `--bind`.
