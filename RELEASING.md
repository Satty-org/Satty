RELEASING
==

Maintainer runbook for cutting a new Tensaku release. Contributors don't
need this — see [CONTRIBUTING.md](CONTRIBUTING.md).

A release goes out over four channels:

1. **GitHub Release** — source tarball, by `release.sh`.
2. **Flatpak bundle** — attached to the release automatically by CI.
3. **crates.io** — `tensaku` + `tensaku_cli`, the one manual step.
4. **AUR** — the `tensaku` package, updated by `release.sh` (separate repo).

`release.sh` handles 1 and 4, CI handles 2 — so the only manual
follow-up is crates.io (3).

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
- Updates the AUR package (see step 4), then prints the crates.io
  publish command as a reminder (step 3).

If the script aborts on a precondition, fix the issue and re-run — it is
safe to re-run as long as the tag does not yet exist.

2. Flatpak bundle — automatic
--

Publishing the GitHub Release triggers `.github/workflows/release.yml`,
which builds the Flatpak bundle from `dev.tensaku.Tensaku.yml` and
attaches `tensaku-vX.Y.Z.flatpak` to the same release. No action needed —
just check the workflow run succeeded.

3. crates.io — the one manual step
--

`release.sh` prints a reminder but does **not** publish to crates.io —
it's deliberately the only manual channel, because crates.io publishes
are permanent (yank-only, no delete). The workspace has two crates and
`tensaku` depends on `tensaku_cli`, so **publish the CLI crate first**:

```sh
cargo publish -p tensaku_cli
cargo publish -p tensaku
```

Both carry the workspace version, so they are already bumped by step 1.
Run this after the tag is pushed so the published crate matches the tag.

4. AUR — automatic
--

`release.sh` updates the AUR package as its last step. The AUR package
is a **separate git repo** — `ssh://aur@aur.archlinux.org/tensaku.git`,
cloned at `~/Code/aur/tensaku` (override with the `TENSAKU_AUR_DIR`
environment variable). The script pulls the clone, bumps
`pkgver`/`pkgrel`, recomputes `sha256sums` from the new source tarball,
regenerates `.SRCINFO`, commits, and pushes. It needs `makepkg` and the
AUR SSH key set up.

It is **best-effort**: if it fails — missing clone, diverged history, no
`makepkg` — the release that already shipped still stands, and the
script tells you to update the AUR by hand.

The rest of the `PKGBUILD` — `depends` and the `package()` install
lines — is **hand-maintained**. `release.sh` only touches the version
fields; when a native dependency changes or a packaged file is renamed,
edit the `PKGBUILD` in the AUR clone directly.

Post-release checklist
--

- [ ] GitHub Release published with the source tarball.
- [ ] `release.yml` workflow succeeded; Flatpak bundle attached.
- [ ] AUR push succeeded — `release.sh` reports it; spot-check the
      [AUR page](https://aur.archlinux.org/packages/tensaku).
- [ ] **crates.io published** — `tensaku_cli` then `tensaku` (the one
      step `release.sh` leaves to you).
- [ ] `https://tensaku.dev` install commands still point at the latest
      version (the `tensaku-site` repo).
