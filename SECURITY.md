# Security policy

## Report a security problem

Do not report a security problem in a public issue, pull request or discussion.

Use GitHub private vulnerability reporting. On the repository page, open the security tab
and select **Report a vulnerability**. Give the tool (`idl-drift` or `idl-verify`), the
version or commit, the exact command, its output, and the input that causes the problem.

## Scope

These are security problems:

- An IDL, a samples file, an RPC response or decoder output that makes a tool crash, hang
  or use all available memory.
- A crafted input that hides a real change from a CI gate: the tool exits 0 when it must
  report the change.

These are not security problems. Report them in a public issue:

- A wrong result on an input that is not made to attack the tool.
- A limit that the documentation already lists: see the limitations of
  [idl-drift](docs/idl-drift.md#limitations) and
  [idl-verify](docs/idl-verify.md#limitations).

## Supported versions

No version is released yet. Fixes go to the `main` branch.
