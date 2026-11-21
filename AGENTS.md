# Lug contributor and agent instructions

These instructions apply to this repository, `0x3st/structured-ai-api`.
Follow the applicable CUHKSZ Dev workflow and quality standards, with the
project-specific rules below. Do not invent Tark requirements, issue links,
review results, or infrastructure dependencies for this standalone project.

## Safe workflow

- Perform every repository change in a new, isolated Git worktree. Never modify,
  reset, clean, or switch branches in the user's existing checkout. Preserve
  uncommitted and untracked work; do not use destructive commands to make a
  checkout appear clean.
- Start ordinary work from the latest remote `main`, on a descriptive branch
  such as `feat/<topic>`, `fix/<topic>`, or `docs/<topic>`.
- Upload code or open a PR only after verification and explicit user approval.
  Release, publish, deploy, and tag operations need separate explicit approval.
- Do not rewrite published history without explicit approval. Before an approved
  rewrite, back up the old history and use `--force-with-lease` bound to the
  exact expected remote SHA. Stop if the remote has changed.
- Remove temporary worktrees and task-owned processes when finished. Temporary
  processes require a hard deadline and cleanup of the entire process tree.
  Use the managed preview tools for local web previews; never leave a bare
  development server running in the background.

## Commit format and content

- Use Conventional Commits: `type(scope): subject`. Both type and scope are
  required. Types: `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`,
  `refactor`, `revert`, `style`, `test`.
- **Write the subject in English**, beginning with a lowercase imperative verb;
  keep it concise and omit the trailing period. Example:
  `fix(engine): preserve precision in nested expressions`.
- Separate the subject and body with a blank line. For substantive changes,
  explain the problem/background, implementation, affected behavior and
  compatibility, and validation actually performed. State meaningful limits,
  such as mock-only coverage or tests not run. Do not claim deployment,
  browser verification, or a fix without evidence.
- Keep commits coherent and reviewable. A PR may contain at most 20 commits;
  squash when appropriate, subject to the history-rewrite safeguards above.
  Check every outgoing commit, not only the tip.
- Use GitHub's verified noreply address for both author and committer. For
  `0x3st`, use `61730913+0x3st@users.noreply.github.com`. Check effective
  identities before committing and again before pushing; never reuse ordinary
  email addresses from old commits.
- Preserve the configured signing requirement and verify the new commit's
  signature. A privacy email is not a signature; do not silently disable signing
  or create/upload signing keys.

## Pull requests

Use a Conventional Commit title with an English subject. Include all five
sections below, with these exact headings and substantive content in each:

```markdown
## 背景
Problem, motivation, and any actual requirement link.

## 改动
Implementation and scope of the changes.

## 影响
Affected modules, APIs, configuration, data, permissions, compatibility,
deployment considerations, and rollback where applicable. Write "None" if none.

## 验证
Commands and scenarios actually run, results, and unverified boundaries.

## 材料
Relevant screenshots, responses, logs, or evidence. Write "None" if inapplicable.
```

- Keep the description synchronized with the current implementation and head.
- Link a real requirement when one exists; do not fabricate a `Part of` line.
- After creating or updating a PR, inspect its current checks and report their
  actual state. No checks configured is not equivalent to checks passing.
- After development and before submitting, ask whether the user wants a
  high-intensity self-review. Do not silently launch one. When authorized,
  follow the CUHKSZ Dev review methodology with independent risk perspectives;
  do not present a limited check as a full review.
- Read the complete PR description, reviews, inline replies, and ordinary
  comments before judging findings. Respect explicit, applicable owner risk
  acceptance unless new evidence invalidates it.
- Show the complete proposed REQUEST_CHANGES review to the user and obtain
  approval before publishing it. Do not bypass this with ordinary comments.
- When addressing review feedback, validate the actual failing path, reply to
  each addressed thread, resolve it when justified, update the description,
  inspect CI, and re-request the relevant reviewers. Remote writes still need
  authorization.

## Implementation and verification

- `crates/lug-engine`: parsing, semantic normalization, AST/prefix evaluation,
  numeric correctness, resource guards, calculation steps, and metering.
- `crates/lug-server`: HTTP/SSE contracts, API keys, GitHub OAuth, sessions,
  authorization, usage, and audit logging.
- `frontend`: React/TypeScript sources, styles, and Playwright tests.
  `web` contains the served build artifacts; regenerate from source rather
  than editing hashed bundles manually.
- `legacy` and `tests/golden`: Python reference implementation and differential
  fixtures. Do not blindly regenerate expected results to hide a regression;
  document intentional semantic deviations and test them independently.
- Keep fixes focused. Validate real entry points and consumers, including error
  paths, precision, limits, state transitions, authentication, user isolation,
  and stream ordering where affected. Update docs and tests with the behavior.
- Before submission, run `git diff --check` and the checks relevant to the change:
  - Rust: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
    --locked -- -D warnings`, and `cargo test --workspace --locked`.
  - Frontend: `cd frontend && npm ci && npm run build`; run the relevant
    Playwright suites for interaction changes using managed preview services.
- Report commands actually run and distinguish automated tests, mock-backed
  UI checks, real integration checks, and manual/browser evidence. Do not
  claim that build success proves runtime behavior.
- Trace user-visible changes across frontend and backend contracts. For visual
  or interaction changes, follow CUHKSZ Dev browser-evidence guidance when
  available. Browser validation must not interfere with the user's desktop
  mouse; skip unsafe interactions and disclose the coverage gap.

## Secrets, dependencies, and licensing

- Never commit API keys, OAuth secrets, `.env` files, runtime `api_keys.json`,
  audit logs, local build caches, or dependency directories. Review staged and
  untracked files before committing; secret scans are supplementary evidence,
  not proof that no secret exists.
- Use stable/GA service APIs, not Beta or Preview APIs, unless an applicable,
  explicit user exception authorizes the actual integration. Review acceptance
  alone does not authorize integration or deployment.
- First-party project code is licensed under MIT; retain the root `LICENSE`.
  Third-party fonts and other assets retain their own licenses and notices,
  including the STIX Two Text SIL Open Font License. Do not relabel third-party
  assets or imply that the MIT license grants trademark rights.
- If an unrelated harness, global toolchain, or infrastructure issue blocks
  work, report its observed impact and request authorization before repairing
  it. Do not expand a project task into an infrastructure repair.
