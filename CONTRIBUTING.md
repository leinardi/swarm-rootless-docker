# Contributing

## Setup

Install [`pre-commit`](https://pre-commit.com/), then install the hooks once with
`make pre-commit-install`. It installs both the `pre-commit` and the `commit-msg` hooks, so commit
messages are checked when you commit. The hooks bring their own tools, shellcheck included;
actionlint also checks the workflows' `run:` scripts when a [`shellcheck`](https://www.shellcheck.net/)
is on `PATH`, as it is in CI.

- `make check` runs the full pre-commit suite on every file, `make check-stage` on the staged files
  only.
- `make help` lists every target.

Read [AGENTS.md](AGENTS.md) for the layout and the conventions a change must keep.

## Commit messages

All commits must follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)
with a scope: `<type>(<scope>)[!]: <description>`. The `conventional-pre-commit` hook enforces this
on `commit-msg`, and the `conventional-commits` CI job checks it again on every pull request.

```text
fix(ci): pin actionlint by SHA
feat(release): attach the build to the release
docs(readme): fix a link
```

The release version is derived from these types, since the last release:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(api)!: drop the v1 endpoints` |
| minor | `feat` | `feat(release): attach the build to the release` |
| patch | `fix` | `fix(ci): pin actionlint by SHA` |
| none | everything else: `build`, `chore`, `ci`, `docs`, `perf`, `refactor`, `style`, `test`, `revert` | `docs(readme): fix a link` |

Pick the type by whether the change should ship in a release, not by what kind of change it is. A
`revert` of a shipped `feat` or `fix` is itself a `fix`. The version tool matches `feat` and `fix`
anywhere before the colon (`prefix:` counts as `fix:`), so keep such words out of other subjects.

Pull requests are merged with merge commits; squash and rebase merging are disabled. Every commit
therefore lands on `main` as it is, so each one needs a correct type, not just the pull request as a
whole.

## Releasing

Dispatch the **Release** workflow from `main`. Leave the version empty to derive it from the
commits, or pass one (e.g. `1.2.0` or `v1.2.0`); tick *dry run* first to see what it would do. The
first release needs an explicit version, since there is no tag to derive it from. It calls
[`simple-tag-and-release`](https://github.com/leinardi/gh-reusable-workflows/blob/main/.github/workflows/simple-tag-and-release.md),
which creates the `vX.Y.Z` tag and the release with generated notes, and moves `vMAJOR` and `latest`
when it is the highest release. A failed run is finished by re-running it: it completes what the
earlier attempt left behind.
