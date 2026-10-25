# template-default

Default @leinardi repository template: the scaffolding every `leinardi/*` repository shares, before
any language or stack is added.

## What it contains

- **Tooling:** a Makefile on [make-common](https://github.com/leinardi/make-common) (`make check`,
  `make pre-commit-install`, `make help`), and a pre-commit config with the core hooks, checkmake,
  actionlint, markdownlint, shellcheck, prettier for YAML, yamllint, and `conventional-pre-commit`
  on `commit-msg` with `--force-scope`.
- **CI:** a `pre-commit-all` job that runs every hook on every file, a `conventional-commits` job
  that checks every commit of a pull request, and a `ci-result` job that aggregates them into the
  one required check, on pull requests and on pushes to `main`. The reviewdog jobs from
  [gha-pre-commit-reviewdog-actions](https://github.com/leinardi/gha-pre-commit-reviewdog-actions)
  comment on pull requests. The pre-commit cache warm-up runs on `main` when the hook config
  changes and weekly, and prunes the older cache entries.
- **Release:** a **Release** workflow on
  [`simple-tag-and-release`](https://github.com/leinardi/gh-reusable-workflows/blob/main/.github/workflows/simple-tag-and-release.md):
  the version is optional and derived from the commits when empty, with a dry run.
- **GitHub:** dependabot for the actions and the hooks, CODEOWNERS, a pull request template, and
  issue templates with blank issues off and a link to the security policy.
- **Docs:** [AGENTS.md](AGENTS.md), [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md),
  and an `adversarial-review` skill in `.agents/skills/` (symlinked as `.claude/skills`).
- **License headers:** `scripts/check-license-headers.sh`, off until the repository has a license
  (see [AGENTS.md](AGENTS.md#license-headers)).

Every action and reusable workflow is pinned by commit SHA, with the version in a comment;
dependabot keeps them current.

## After creating a repository from this template

1. Replace `template-default` everywhere it names this repository: `git grep -n template-default`
   lists every place, including the security link in `.github/ISSUE_TEMPLATE/config.yml`.
2. Rewrite this README and the "What this is" and "Layout" sections of `AGENTS.md` for the project.
3. Add a `LICENSE`. The template has none on purpose: the repositories do not all use the same one.
   Then enable the license-header check, as [AGENTS.md](AGENTS.md#license-headers) describes, or
   delete `scripts/check-license-headers.sh` and its mentions in the docs.
4. Add the stack, following the repository closest to it (e.g. swarm-scheduler-exporter for Go,
   adversarial-review-loop for Python):
    - its pre-commit hooks, the toolchains they need in `pre-commit-all`, and the matching reviewdog
      and test jobs in `ci.yaml`, each gating job added to `ci-result`'s `needs`;
    - its dependabot ecosystem, with the `fix(deps)` prefix for dependencies that ship with a
      release and `chore(deps)` for the rest;
    - make-common snippets in `MK_COMMON_FILES`, and project targets in a local `.mk/<name>.mk`
      listed in `MK_LOCAL_FILES`, never as recipes in the Makefile;
    - its sections in `.editorconfig` and `.prettierignore`.
5. Keep, extend or delete `release.yaml`: pass `artifact_name` to attach built files, replace it
   with a stack-specific release (e.g. swarm-scheduler-exporter's for images and binaries), or
   delete it, and the release sections of the docs, if the project never releases. The first release
   needs an explicit version, since there is no tag to derive it from.
6. Tailor `.agents/skills/adversarial-review/SKILL.md`: replace "template-default" with the
   project's name in its description and title, then adapt its invariants and verification gates.
7. Run `make pre-commit-install`, then `make check`.
8. Add the repository to gh-leinardi-iac, with `ci-result` as its required check, and enable
   private vulnerability reporting, which `SECURITY.md` points to.
