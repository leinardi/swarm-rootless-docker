# AGENTS.md

## What this is

The default template for `leinardi/*` repositories: the shared scaffolding (Makefile, pre-commit
hooks, CI, release, GitHub settings files, docs, skills) with no language or stack yet. Repositories
created from it copy these files once; a change here reaches only the repositories created
afterwards, so a fix that existing repositories need is ported to them separately.

## Common commands

```bash
make check               # pre-commit on all files: the local gate, as CI's pre-commit-all
make check-stage         # pre-commit on the staged files only
make pre-commit-install  # installs the pre-commit and commit-msg hooks
make mk-common-update    # refresh the shared .mk snippets from leinardi/make-common@v1
make help                # lists every target
```

Before calling a change done, run `make check`: checkmake, actionlint, markdownlint, shellcheck,
prettier and yamllint. In CI, `ci-result` is the one required check: it needs `pre-commit-all` and
`conventional-commits` (`ci.yaml`); the reviewdog jobs only annotate pull requests.

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run. Project
targets live in the local `.mk/*.mk` files listed in `MK_LOCAL_FILES`, never as recipes in the
Makefile.

## Layout

| Path | What lives there |
| --- | --- |
| `Makefile`, `.mk/`, `scripts/bootstrap-mk-common.sh` | make-common: shared snippets in `MK_COMMON_FILES`, local ones in `MK_LOCAL_FILES` |
| `.github/workflows/ci.yaml` | `pre-commit-all`, `conventional-commits` and the `ci-result` aggregator; the reviewdog jobs on pull requests |
| `.github/workflows/release.yaml` | the **Release** workflow, on gh-reusable-workflows' `simple-tag-and-release` |
| `.github/workflows/pre-commit-warmup.yaml` | refreshes the pre-commit cache on `main` (hook config change, weekly) and prunes the older entries |
| `scripts/check-license-headers.sh` | the license-header check, off until the repository has a license ([License headers](#license-headers)) |
| `.agents/skills/` | project skills; `.claude/skills` is a symlink to it |

### Generated or vendored: never hand-edit

- `.mk/help.mk`, `.mk/pre-commit.mk`, `.mk/password.mk` and `.mk/.mk-common-version`: fetched from
  `leinardi/make-common`, refreshed with `make mk-common-update`.
- `scripts/bootstrap-mk-common.sh`: also from make-common; a change is ported upstream.

## Conventions

- Pin every action and reusable workflow by full commit SHA, with the version in a trailing comment
  (`@<sha> # vX.Y.Z`).
- Give every workflow a top-level `permissions: contents: read`, and widen it only on the job that
  needs more.
- Pass `${{ }}` expressions into `run:` scripts through `env:`, never inline them in the script.
- Name workflow files `.yaml`, except a file whose name a trusted publisher (PyPI, npm) is bound to.
- A new gating CI job is added to `ci-result`'s `needs`. A hook that needs a system tool gets that
  tool installed in `pre-commit-all`; a hook that a dedicated job runs instead is listed in
  `pre-commit-all`'s `SKIP`, with the job named in a comment.
- `README.md` is the user-facing contract: keep it in step with what the repository does.

## License headers

The template has no `LICENSE`, so nothing checks headers here. `scripts/check-license-headers.sh`
is ready for the repository that picks one; until it is configured it refuses to run. To enable it:

1. Add the `LICENSE`, and fill in the configuration block at the top of the script:
   `PROJECT_NAME`, `COPYRIGHT_HOLDER`, `LICENSE_NAME` and `REQUIRED_LINES` (presets for MIT,
   Apache-2.0 and GPL-3.0-or-later are in its comments). Trim `GOVERNED_EXTENSIONS` and
   `GOVERNED_NAMES` to the stack if needed, and exempt generated and vendored files in
   `EXEMPT_PREFIXES` or `EXEMPT_PATHS`, each for a reason.
2. Add the header to every file the script lists, the script itself included (after the shebang),
   with the year the file was first committed:
   `git log --diff-filter=A --follow --format=%ad --date=format:%Y -- <file> | tail -1`. An
   IntelliJ copyright profile in `.idea/copyright/` (which `.gitignore` tracks) keeps new files
   right.
3. Wire the hook into the `local` repo of `.pre-commit-config.yaml`. It reads the whole tree, so it
   always runs; `stages` pins it to one stage, because `always_run` makes a hook eligible at every
   installed stage, and `commit-msg` is one of them:

   ```yaml
   - id: check-license-headers
     name: license headers
     entry: scripts/check-license-headers.sh
     language: system
     pass_filenames: false
     always_run: true
     stages: [pre-commit]
   ```

4. Rewrite this section to say what the check enforces, and its validation matrix row to run it.

Governed by default: code (shell, Python, Go, TypeScript, JavaScript, Vue, Rust, Ruby, Terraform,
Lua, Dockerfiles, the Makefile and the repo-local `.mk` fragments). Never YAML, JSON or Markdown.
The make-common files are exempt: they carry make-common's own header.

## Markdown

Prose wraps at 100 columns (markdownlint `line-length`, `.editorconfig`); tables, code blocks and
headings may run longer. Never wrap inside a table, a code block or a link target.

## Commit messages

All commits MUST be Conventional Commits 1.0.0 **with a scope**:
`<type>(<scope>)[!]: <description>`, optional blank-line body and footers. Enforced by the
`conventional-pre-commit` `commit-msg` hook (`--force-scope`, installed by
`make pre-commit-install`) and by the `conventional-commits` CI job. Types: `feat`, `fix`, `docs`,
`test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`, `revert`. Breaking changes use `!`
before `:` or a `BREAKING CHANGE:` footer. Release notes are not built from these messages:
`gh release create --generate-notes` lists the merged pull requests by title. Examples:
`fix(ci): pin actionlint by SHA`, `feat(release): attach the build to the release`.

A release with no explicit version is derived by `svu` from the commits since the last `vX.Y.Z` tag,
so a wrong type ships a wrong version:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(api)!: drop the v1 endpoints` |
| minor | `feat` | `feat(release): attach the build to the release` |
| patch | `fix` | `fix(ci): pin actionlint by SHA` |
| none | everything else: `perf`, `refactor`, `build`, `ci`, `chore`, `docs`, `style`, `test`, `revert` | `docs(readme): fix a link` |

The highest bump among the commits wins; with only "none" commits since the last tag, a release
with no version fails with "nothing to bump".

**Pick the type by whether the change should ship, not by what kind of change it is.** Anything
that changes what the project ships and that users should receive is `fix` (or `feat`), even when
it is a performance improvement, a refactor or a revert. Use `perf`, `refactor`, `style` and
`revert` only when the commit is deliberately not meant to trigger a release on its own. A `revert`
of a shipped `feat` or `fix` is itself a `fix`. `svu` matches `feat`/`fix` anywhere in the subject
(e.g. `prefix:` counts as `fix:`), so avoid a word ending in `feat` or `fix` directly before a colon
in other subjects. PRs land as merge commits, so every commit counts, not just the PR title. See
[`CONTRIBUTING.md`](CONTRIBUTING.md#releasing).

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `.github/workflows/**`, `.github/dependabot.yml` | `pre-commit run actionlint --all-files`; reason through each trigger the workflow has (pull request, push, dispatch, schedule) |
| `.pre-commit-config.yaml` | `make check`; a hook that needs a system tool also gets it in `ci.yaml`'s `pre-commit-all` |
| `Makefile`, the `.mk/*.mk` files in `MK_LOCAL_FILES` | `make check` (checkmake), `make help` |
| `scripts/bootstrap-mk-common.sh`, the make-common `.mk` files | never by hand: port the change to make-common, then `make mk-common-update` and `make help` |
| `scripts/check-license-headers.sh` | `make check` (shellcheck); once configured, run it, and see a file without the header fail it |
| `**/*.md`, `.agents/skills/**` | `make check` (markdownlint); prose wrapped at 100 columns |

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load `adversarial-review` for any
review request ("review my diff", "is this ready to merge").
