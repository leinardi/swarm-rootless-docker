# Contributing

## Setup

You need:

- a Rust toolchain via [rustup](https://rustup.rs/); `rust-toolchain.toml` pins the version, and
  rustup installs it on first use;
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) for the dependency checks
  (`cargo install --locked cargo-deny`);
- [`pre-commit`](https://pre-commit.com/). Install the hooks once with `make pre-commit-install`:
  it installs both the `pre-commit` and the `commit-msg` hooks, so commit messages are checked
  when you commit.

Then:

- `make verify` builds, checks formatting, runs clippy and runs every test, the meta-tests
  included;
- `make check` runs the full pre-commit suite on every file, `make check-stage` on the staged
  files only;
- `make help` lists every target.

Read [AGENTS.md](AGENTS.md) for the layout, the coding rules and the validation matrix a change
must pass.

## Commit messages

All commits must follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)
with a scope: `<type>(<scope>)[!]: <description>`. The `conventional-pre-commit` hook enforces this
on `commit-msg`, and the `conventional-commits` CI job checks it again on every pull request.

```text
fix(launcher): stop the daemon by ID
feat(image): add the uid 1000 variant
docs(readme): fix a link
```

The release version is derived from these types, since the last release:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(launcher)!: rename the --data-root flag` |
| minor | `feat` | `feat(image): add the uid 1000 variant` |
| patch | `fix` | `fix(launcher): stop the daemon by ID` |
| none | everything else: `build`, `chore`, `ci`, `docs`, `perf`, `refactor`, `style`, `test`, `revert` | `docs(readme): fix a link` |

Pick the type by whether the change should ship in a release, not by what kind of change it is. A
`revert` of a shipped `feat` or `fix` is itself a `fix`. The version tool matches `feat` and `fix`
anywhere before the colon (`prefix:` counts as `fix:`), so keep such words out of other subjects.

Pull requests are merged with merge commits; squash and rebase merging are disabled. Every commit
therefore lands on `main` as it is, so each one needs a correct type, not just the pull request as a
whole.

## Releasing

Nothing is released yet. The image release workflow (GHCR, per Docker major and UID, tested and
scanned before it is tagged) lands together with the launcher and the image, and its procedure
will be documented here and in `docs/release.md`. The first release needs an explicit version,
since there is no tag to derive it from.
