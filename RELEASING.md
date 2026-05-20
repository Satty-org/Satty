# Releasing tensaku

Releases are automated by [release-plz](https://release-plz.dev) — there
is no release script to run.

## Cutting a release

1. Land changes on `main` using [Conventional Commits](https://www.conventionalcommits.org)
   (`feat:`, `fix:`, `feat!:` …) — release-plz reads them to pick the
   next version.
2. release-plz keeps a **release PR** open that bumps the workspace
   version and updates `CHANGELOG.md`. Review it.
3. **Merge the release PR.** release-plz then:
   - publishes `tensaku` and `tensaku_cli` to crates.io in dependency
     order;
   - creates the `vX.Y.Z` tag and the GitHub Release.
4. The GitHub Release fires the packaging workflows automatically:
   - `release.yml` — Linux x86_64 tarball + Flatpak bundle.
   - `aur-publish.yml` — pushes the updated package to the AUR.

No local steps, no `release.sh`.

## One-time setup

Repository secrets (Settings → Secrets and variables → Actions):

| Secret | Purpose |
| --- | --- |
| `RELEASE_PLZ_TOKEN` | Fine-grained PAT (`contents: write`, `pull-requests: write`). Required so release-plz's tag/Release events trigger the packaging workflows — the default `GITHUB_TOKEN` cannot. |
| `CARGO_REGISTRY_TOKEN` | crates.io token scoped to publish the `tensaku*` crates. |
| `AUR_SSH_KEY` | Private SSH key registered with the AUR account that maintains `tensaku`. |

## The AUR package

`packaging/aur/PKGBUILD` is the source of truth. `aur-publish.yml`
copies it, pins `pkgver`/`pkgrel`, refreshes `sha256sums` with
`updpkgsums`, regenerates `.SRCINFO`, and pushes to
`ssh://aur@aur.archlinux.org/tensaku.git`. Edit `depends`, `package()`,
etc. in `packaging/aur/PKGBUILD`; never edit the AUR repo directly.

## Re-running a step

`aur-publish.yml` has a `workflow_dispatch` trigger with a `tag` input
so a failed AUR push can be re-run by hand against an existing release
tag:

```sh
gh workflow run aur-publish.yml -f tag=vX.Y.Z
```
