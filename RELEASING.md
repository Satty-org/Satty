RELEASING
==

Maintainer runbook for cutting a new Tensaku release. Contributors don't
need this — see [CONTRIBUTING.md](CONTRIBUTING.md).

A release goes out over four channels:

1. **GitHub Release** — source tarball, scripted.
2. **Flatpak bundle** — attached to the release automatically by CI.
3. **crates.io** — `tensaku` + `tensaku_cli`, published manually.
4. **AUR** — the `tensaku` package, updated manually in a separate repo.

Steps 1–2 are one command. Steps 3–4 are short manual follow-ups.

Before you start
--

- Work from a **clean working tree on `main`** with `main` up to date.
- `gh`, `make`, and `cargo` must be installed, and `gh` authenticated
  (`gh auth login`). `release.sh` checks all of this and aborts otherwise.
- Pick the next version as plain `X.Y.Z` semver (no `v` prefix).

1. GitHub Release — `release.sh`
--

```sh
./release.sh 0.22.0
```

The script does everything for the GitHub release:

- Bumps `workspace.package.version` in `Cargo.toml`, and the matching
  `tensaku_cli` path-dependency version requirement.
- Refreshes `Cargo.lock` (`cargo generate-lockfile`).
- Replaces the `NEXTRELEASE` placeholder with the new version in
  `cli/src/command_line.rs`, `src/configuration.rs`, `config.toml`,
  and `README.md`.
- Shows the diff and **waits for you to confirm** before committing.
- Commits `Release vX.Y.Z`, creates an annotated tag, and pushes both
  `main` and the tag to GitHub.
- Builds the release tarball (`make package`).
- Publishes the GitHub Release with auto-generated notes and the
  tarball attached.

If the script aborts on a precondition, fix the issue and re-run — it is
safe to re-run as long as the tag does not yet exist.

2. Flatpak bundle — automatic
--

Publishing the GitHub Release triggers `.github/workflows/release.yml`,
which builds the Flatpak bundle from `dev.tensaku.Tensaku.yml` and
attaches `tensaku-vX.Y.Z.flatpak` to the same release. No action needed —
just check the workflow run succeeded.

3. crates.io
--

Not handled by `release.sh`. The workspace has two crates and `tensaku`
depends on `tensaku_cli`, so **publish the CLI crate first**:

```sh
cargo publish -p tensaku_cli
cargo publish -p tensaku
```

Both carry the workspace version, so they are already bumped by step 1.
Run this after the tag is pushed so the published crate matches the tag.

4. AUR
--

The AUR package lives in a **separate git repo**, not in this one:
`ssh://aur@aur.archlinux.org/tensaku.git` (maintainer keeps a clone at
`~/Code/aur/tensaku`). It is never nested inside this repo.

In that clone:

```sh
git pull                                    # in case of out-of-band edits
# edit PKGBUILD: set pkgver=0.22.0, reset pkgrel=1
updpkgsums                                  # refresh sha256sums from the new tarball
makepkg --printsrcinfo > .SRCINFO           # regenerate .SRCINFO
makepkg -f                                  # verify it builds clean
git add PKGBUILD .SRCINFO
git commit -m "Update to 0.22.0"
git push
```

`makepkg` leaves `pkg/`, `src/`, and `*.tar.*` artifacts behind; the
repo's `.gitignore` keeps them out of commits — only `PKGBUILD` and
`.SRCINFO` are tracked.

Post-release checklist
--

- [ ] GitHub Release published with the source tarball.
- [ ] `release.yml` workflow succeeded; Flatpak bundle attached.
- [ ] `tensaku_cli` and `tensaku` published to crates.io.
- [ ] AUR package updated and pushed.
- [ ] `https://tensaku.dev` install commands still point at the latest
      version (the `tensaku-site` repo).
