RELEASING
==

Maintainer runbook for cutting a new Tensaku release. Contributors don't
need this — see [CONTRIBUTING.md](CONTRIBUTING.md).

A release goes out over four channels — `release.sh` handles 1, 3, and
4; CI handles 2. The whole thing is one command:

1. **GitHub Release** — source tarball.
2. **Flatpak bundle** — attached to the release automatically by CI.
3. **crates.io** — `tensaku` + `tensaku_cli`, token read from 1Password.
4. **AUR** — the `tensaku` package (a separate repo).

Before you start
--

- Work from a **clean working tree on `main`** with `main` up to date.
- `gh`, `make`, `cargo`, and `op` (1Password CLI) must be installed,
  and `gh` authenticated (`gh auth login`). `release.sh` checks all of
  this and aborts otherwise.
- The crates.io token must live in 1Password at
  `op://Private/crates.io/tensaku-release` — `release.sh` reads it
  via `op` at the start of the run, so nothing is stored on disk.
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
- Publishes both crates to crates.io (step 3) and updates the AUR
  package (step 4).

If the script aborts on a precondition, fix the issue and re-run — it is
safe to re-run as long as the tag does not yet exist.

2. Flatpak bundle — automatic
--

Publishing the GitHub Release triggers `.github/workflows/release.yml`,
which builds the Flatpak bundle from `dev.tensaku.Tensaku.yml` and
attaches `tensaku-vX.Y.Z.flatpak` to the same release. No action needed —
just check the workflow run succeeded.

3. crates.io — automatic
--

`release.sh` publishes both crates after the GitHub Release. The
workspace has two crates and `tensaku` depends on `tensaku_cli`, so the
script publishes **`tensaku_cli` first** — `cargo publish` waits for it
to land in the registry index, then publishes `tensaku` against it.

The crates.io token is **read from 1Password** at the start of the run
(`op read op://Private/crates.io/tensaku-release`) and passed to
`cargo publish` via `CARGO_REGISTRY_TOKEN` — it never touches disk and
isn't taken from `~/.cargo/credentials.toml`. Use a token scoped to
`publish-update` on the `tensaku*` crates.

Unlike the AUR step, this is **fatal**: a `cargo publish` failure aborts
the script (the GitHub Release and AUR update have already shipped).
crates.io publishes are permanent — yank-only, no delete — so recovering
from a partial publish needs care: if `tensaku_cli` already went up,
re-run by publishing only `tensaku` by hand.

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
- [ ] crates.io publish succeeded — `release.sh` reports it; spot-check
      [crates.io/crates/tensaku](https://crates.io/crates/tensaku).
- [ ] `https://tensaku.dev` install commands still point at the latest
      version (the `tensaku-site` repo).
