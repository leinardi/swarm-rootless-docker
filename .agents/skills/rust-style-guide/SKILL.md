---
name: rust-style-guide
description: >
  Rust coding rules for swarm-rootless-docker: the workspace lint set (clippy pedantic plus the
  restriction lints) and how to satisfy it, #[expect] with a reason as the only suppression, the
  maintainer's rules (descriptive names, exhaustive matches, public items first, no panics or
  printing outside tests), and the launcher's patterns (the Docker API behind one trait, the
  fixed daemon spec, fail-closed validation, ownership by ID, one shutdown budget, the hand-built
  runtime, logging and tests). Use when writing, editing or reviewing any .rs file in
  swarm-rootless-docker; consult it before generating Rust code, not after clippy fails.
---

# Rust Style Guide — swarm-rootless-docker

Rules from the root `Cargo.toml` (`[workspace.lints]`), `clippy.toml`, `rustfmt.toml` and the
existing code in `src/`, `tests/` and `meta/`. §1–§4 are what the lints enforce, §5–§7 are the
maintainer's rules, §8–§11 are the launcher's patterns, §12–§14 are review rules.

Every lint and test command runs workspace-wide, so both packages (`swarm-rootless-docker` and
`meta`) are held to the same set: `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo fmt --all --check`. Never commit with `--no-verify`;
fix the underlying issue instead.

---

## 1. The lint set

`[workspace.lints]` is declared once, and every member opts in with `[lints] workspace = true`
(a member does not inherit it otherwise; `meta/tests/workspace_lints.rs` fails when one does not).
`clippy.toml` sits at the workspace root, so its settings apply to both packages.

| Lint | Level | What to do |
| --- | --- | --- |
| `unsafe_code` | forbid | No `unsafe`, anywhere; reach for a safe crate (`rustix`) for syscalls |
| `clippy::pedantic` | warn (priority -1), an error under `-D warnings` | Satisfy it; the usual ones below |
| `allow_attributes`, `allow_attributes_without_reason` | deny | `#[expect(lint, reason = "...")]` only (§2) |
| `unwrap_used`, `expect_used` | deny | Return an error; tests may use both (§3) |
| `print_stdout`, `print_stderr` | deny | Log through `tracing`; `writeln!` to a handle when output is the point (`--version`, the probe's JSON report) |
| `wildcard_enum_match_arm` | deny | Name every variant (§6) |
| `min_ident_chars` | deny | Descriptive names, no single-letter identifiers (§5) |

The pedantic lints that come up most:

- `must_use_candidate`: put `#[must_use]` on a pure function or constructor that returns a value.
- `missing_errors_doc`, `missing_panics_doc`: a public function returning `Result` has an
  `# Errors` section; one that can panic has `# Panics` (better: make it not panic).
- `doc_markdown`: identifiers in doc comments go in backticks (`` `container_id` ``).
- `too_many_lines` (100): split the function into named steps.
- `needless_pass_by_value`, `trivially_copy_pass_by_ref`: borrow what you only read; pass small
  `Copy` types by value.
- `cast_possible_truncation`, `cast_sign_loss`: use `try_from` or `From`; never an `as` cast on a
  UID, GID, range bound or byte size, where a wrap is a security bug.

`rustfmt.toml` sets `edition = "2024"` and `max_width = 100`. The `rustfmt-on-edit.sh` hook
formats a `.rs` file after every edit in Claude Code, and the `cargo-fmt` pre-commit hook checks.

## 2. Suppressions: `#[expect]` with a reason, counted

- Only `#[expect(clippy::lint, reason = "...")]`. `#[allow]` is denied, and so is either
  attribute without a reason. Unlike `#[allow]`, an `#[expect]` fails the build once the lint
  stops firing, so a stale suppression cannot linger.
- Put it on the smallest item that needs it: the statement or the function, not the module. A
  file-level `#![expect(...)]` is for an integration-test file whose helpers all share the reason
  (`expect_used` in helpers that are not `#[test]` functions, which clippy's test allowance does
  not cover).
- The reason says why the fix does not apply here, not which lint fired.
- Suppressions are ratcheted: `scripts/check-suppressions.sh` counts every `#[expect]` and
  `#[allow]` per lint against `.suppressions-baseline` and refuses a commit that adds one without
  changing the baseline. After adding, moving or removing one: stage it, run
  `scripts/check-suppressions.sh --update`, and commit the baseline with it. Raising a count
  needs a reviewer's agreement.

## 3. Errors

- One `thiserror` enum per module, with `#[source]` on the underlying error and the context in
  the variant's fields. There is no `anyhow`.
- The message starts with the module and names the operation and its subject:
  `#[error("launcher: remove stale daemon {container_id}: {source}")]`. A configuration error
  names the flag and the bad value: `--data-root: not an absolute path: data`.
- No `unwrap`/`expect` outside tests. A `Mutex` that may be poisoned is read with
  `.lock().unwrap_or_else(PoisonError::into_inner)` only when the data is still consistent; say
  so in a comment.
- Exit codes: the daemon's own status when it exits, `1` for a launcher error, `2` for a usage or
  configuration error. `main` is the only place that exits the process.

## 4. Documentation and comments

- Every module starts with a `//!` comment saying what it owns; every public item has a `///`
  comment starting with what it is or does.
- Every `.rs` file starts with the MIT license block (`Copyright (c) 2026 Roberto Leinardi`, the
  year of the file's first commit), then the module comment.
- A comment says **why the code is the way it is**; history and "changed X to Y" belong in the
  commit body.
- Where the port deliberately differs from swarm-device-access's Go launcher, the module comment
  names the difference (the intended changes are listed in the plan and in
  `docs/architecture.md`). Any other difference from the Go behaviour is a defect.
- `TODO` is allowed; a comment on code you did not otherwise change is not.

## 5. Names

- `min_ident_chars` refuses single-character names. Name a binding for what it holds at any
  distance: `index`, `state`, `container_id`, `owner_id`, `range_start`, never `i`, `s`, `id`.
  Closure parameters too (`|container| ...`, not `|c| ...`).
- Error bindings name their source when several are in scope: `inspect_error`, `parse_error`.
- An unused parameter is `_`, or `_name` when the name documents it.

## 6. Matches, item order, types

- **Exhaustive matches.** A `match` on an enum names every variant (`wildcard_enum_match_arm`);
  group them with `|` when they share an arm. A new variant is then a compile error everywhere it
  matters. Matching on integers or strings may keep a `_` arm, and that arm refuses.
- **Item order.** Within an `impl`, public items come before private ones. Not linted: checked
  in review.
- **Types over strings.** A validated value gets a type whose constructor is the only way to make
  one (`InstanceName`, `HostPath`, `IdRange`), so unvalidated input cannot reach the spec.
- **Seams.** Code that talks to Docker takes the `DockerApi` trait, never the bollard client
  directly; production passes the bollard implementation, tests the recording fake.

## 7. Reuse before writing

As modules land, list each helper here with what it replaces (`Need | Use | Not`). Until then:
check `src/` for an existing parser, validator or Docker call before adding a second one, and
prefer extending the `DockerApi` trait over a side channel to the client.

## 8. Fail closed

- Every flag, label and probe result is untrusted input. An empty, unknown or malformed value
  refuses with an error naming it; it never falls back to a default that widens what the daemon
  container gets or what the launcher may remove.
- The daemon container's spec is built in one function from validated types only, and a unit test
  pins the whole `HostConfig` and `Config`. A new mount, capability, device, security option or
  namespace setting shows up as a change to that test, and needs the README and `SECURITY.md` to
  change in the same commit.
- `Mounts`, never `Binds`: a missing source must fail the create, not make Docker create an empty
  directory on the host.
- Only a **confirmed** absence counts: a stale daemon or lock is removed when its owner's inspect
  returns NotFound or a not-running state. Any other error, a nil state or a foreign container
  refuses.
- Containers are stopped and removed **by ID**, never by name, so an old launcher can never touch
  a newer launcher's daemon. Listings that decide reservations use `all = true`.

## 9. Docker streams and shutdown

- The tokio runtime is built by hand in `main` (not `#[tokio::main]`) and ended with
  `shutdown_timeout(remaining)`, so a call blocked on the socket cannot hold the process open.
- Request/response calls get their own timeout (`DOCKER_CALL_TIMEOUT`). Stream setups (attach,
  wait) are raced against a timeout and against cancellation, and a late result is released in
  the background, never awaited.
- Streams run on a context detached from the signal, so the last log lines and the exit status
  are not lost on SIGTERM.
- Shutdown has one budget from the signal: each step (stop, wait, remove, drain) gets only what
  remains, never a fresh grace period per step.

## 10. Logging

- `tracing` macros only, through the subscriber `main` installs before anything else logs
  (tracing drops every event before a subscriber exists).
- Structured fields, never `format!` in the message:
  `warn!(container_id = %short_id(&id), error = %remove_error, "launcher: remove failed")`. Use
  `%` for `Display`, `?` only when the `Debug` form is what a reader needs.
- Messages start with the module (`"launcher: daemon started"`).
- The daemon's own output is forwarded byte-for-byte to the launcher's stdout and stderr, not
  re-logged through tracing.
- Never log a secret. Labels, paths, UIDs and container IDs are not secrets; registry credentials
  would be.

## 11. Tests

- Unit tests sit beside the code: a `#[cfg(test)] mod tests` at the end of the file, or a
  `tests.rs` in the module directory. Integration tests against a real Docker daemon are in
  `tests/`, gated by `SRD_IT=1` (with `SRD_IT_REQUIRE=1`, set in CI, a missing Docker fails
  instead of skipping); repository meta-tests are in `meta/tests/`.
- A test ported from swarm-device-access carries a doc comment naming the Go test it replaces:
  ``/// Go: `TestRun_CallOrder` ``. A Go test that is not ported is listed with the reason.
- Deterministic over timed: a race window is built by the test (a barrier in the fake, a
  hand-made container in the integration test), never hit by sleeping and hoping.
- Time-dependent tests run on a paused runtime (`#[tokio::test(start_paused = true)]`). Never
  `std::thread::sleep`.
- Waiting, by class:
  - **Positive eventual:** poll or await a channel under `tokio::time::timeout`, and fail naming
    what never happened. Under a paused clock a wait with no timeout can spin forever.
  - **Negative assertion:** give the wrong behaviour a bounded window, then assert it did not
    happen, and say so in a comment.
  - **Real elapsed time:** only when the duration is the point (the shutdown budget), as a named
    constant.
- Logs: a scoped `set_default` subscriber alone loses records under the parallel test runner
  (tracing caches callsite interest across threads); install a global catch-all first.
- A meta-test gated on a tool returns early when the tool is missing, unless
  `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1`, which turns it into a failure (CI sets it).

## 12. What to avoid

- `unwrap`/`expect`, `println!`/`eprintln!` or `unsafe` outside the places above.
- `#[allow]`, or an `#[expect]` whose reason restates the lint (§2).
- A `_ =>` arm on an enum (§6).
- A daemon spec field that comes from anything but a validated type (§8).
- Stopping or removing by name, or deciding a reservation from a listing without `all = true`.
- A `Regex` compiled where it is used: a fixed pattern is a `static NAME: LazyLock<Regex>` beside
  its user, compiled once per process, and a pattern built from runtime data is compiled once,
  outside the loop that uses it. A helper that returns a fresh `Regex` per call is the trap when it
  is called per line or per file: in an unoptimised test build, one such helper in
  swarm-device-access's meta-tests took 27 s of a 36 s `cargo test`.
- Designing for hypothetical requirements: no configurability or abstractions for features that
  do not exist yet. A new launcher flag is a widening of the trust boundary and needs a reason.
- Skipping or suppressing pre-commit hooks (`--no-verify`).

## 13. Quick checklist before submitting Rust code

- [ ] `cargo fmt --all` clean; clippy clean with `--workspace --all-targets -- -D warnings`
- [ ] No `unwrap`/`expect`/printing outside tests; errors are the module's `thiserror` enum
- [ ] Each `#[expect]` has a reason saying why the fix does not apply; baseline updated (§2)
- [ ] Descriptive names; every enum variant matched by name
- [ ] Public items before private ones in each `impl`
- [ ] `#[must_use]`, `# Errors` and backticked identifiers where pedantic asks
- [ ] Every input validated into a type; every refusal names its input (§8)
- [ ] The pinned spec test changed only if the profile deliberately changed, with the docs
- [ ] Docker calls through `DockerApi`, by ID, with timeouts; one shutdown budget (§9)
- [ ] Ported tests carry their `Go:` comment; no timing races, no unbounded waits

## 14. Lint and test loop

1. `make rust-fmt` (or let the edit hook do it).
2. `make verify`: `rust-build`, `rust-fmt-check`, `rust-clippy` and `rust-test` over the
   workspace, the meta-tests included.
3. Fix each report and re-run until clean. For a single test:
   `cargo test --locked -p swarm-rootless-docker <name>`.
4. `make check-stage` (or `make check`), then `git status`: hooks may rewrite files in place. If
   you added, moved or removed a suppression, run `scripts/check-suppressions.sh --update` and
   stage `.suppressions-baseline`.
