#!/usr/bin/env bash
#
# release.sh — cut a new tensaku release.
#
# Run from a clean working tree on `main`. With no argument it
# suggests the next version from the commits since the last release
# and asks you to confirm; pass an explicit X.Y.Z to override:
#
#   ./release.sh            # suggest the next version, then confirm
#   ./release.sh 0.25.0     # use an explicit version
#
# The script bumps the version, refreshes Cargo.lock, fills in the
# NEXTRELEASE placeholder, commits + tags, pushes to GitHub, builds
# the release tarball, publishes the GitHub Release, updates the AUR
# package, and publishes the crates to crates.io. The crates.io token
# is read from 1Password via the `op` CLI — nothing is stored on disk.

set -euo pipefail

say() { printf '\n\033[1;33m++ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m!! %s\033[0m\n' "$*" >&2; exit 1; }

#--- arguments -----------------------------------------------------------------

# The version is optional. With no argument the script suggests the
# next one from the commits since the last release (see "pick the new
# version" below); pass an explicit X.Y.Z to override the suggestion.
NEW_VER="${1:-}"
NEW_VER="${NEW_VER#v}"
# Fail fast on a malformed explicit argument, before the slow `op`
# auth in preconditions. An empty NEW_VER is filled in — and validated
# — by "pick the new version" below.
[[ -z "$NEW_VER" || "$NEW_VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "version '$NEW_VER' is not a plain X.Y.Z semver"

GH_REPO="jondkinney/tensaku"

cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

#--- preconditions -------------------------------------------------------------

say "checking preconditions"

for tool in gh make cargo op; do
    command -v "$tool" >/dev/null || die "$tool is not installed"
done
gh auth status >/dev/null 2>&1 || die "gh is not authenticated — run 'gh auth login'"

# crates.io token, read from 1Password up front so an `op` auth
# failure aborts before anything is published. Held in a plain
# (un-exported) variable — only the `cargo publish` calls below see it.
CRATESIO_TOKEN="$(op read 'op://Private/crates.io/tensaku-release')" \
    || die "couldn't read the crates.io token from 1Password (op)"
[[ -n "$CRATESIO_TOKEN" ]] || die "the crates.io token from 1Password is empty"

[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] \
    || die "not on the main branch — check out main first"
[[ -z "$(git status --porcelain)" ]] \
    || die "working tree is not clean — commit or stash your changes first"

# Read the version from the [workspace.package] section specifically.
# A bare `^version =` match would grab the first dependency table's
# version line (e.g. [dependencies.relm4-icons]) — same section scope
# as the bump sed below.
CUR_VER="$(awk -F'"' '
    /^\[workspace\.package\]/ { in_wp = 1; next }
    /^\[/                     { in_wp = 0 }
    in_wp && /^version = "/   { print $2; exit }
' Cargo.toml)"
[[ "$CUR_VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "couldn't read a valid current version from Cargo.toml"

#--- pick the new version ------------------------------------------------------

# With no version argument, suggest the next one from the Conventional
# Commit types landed since the current release was tagged. Standard
# SemVer mapping, highest bump any commit calls for wins: a breaking
# change (a `type!:` subject or a `BREAKING CHANGE` footer) bumps
# major, a `feat` bumps minor, anything else bumps patch.
if [[ -z "$NEW_VER" ]]; then
    # "Since the last release" is measured from the tag for the
    # current version; fall back to the newest v* tag if it's missing.
    base_ref="v$CUR_VER"
    git rev-parse -q --verify "refs/tags/$base_ref" >/dev/null \
        || base_ref="$(git tag --list 'v*' --sort=-v:refname | head -1)"
    [[ -n "$base_ref" ]] || die "no release tag to compare against — pass the version explicitly"

    range="$base_ref..HEAD"
    count="$(git rev-list --count "$range")"
    [[ "$count" -gt 0 ]] || die "no commits since $base_ref — nothing to release"

    # Subjects carry the type and the optional breaking `!`; bodies
    # carry a `BREAKING CHANGE:` footer. `grep -c` exits non-zero on
    # zero matches, hence the `|| true`.
    subjects="$(git log --format='%s' "$range")"
    n_feat="$(grep -cE '^feat(\([^)]*\))?!?:' <<<"$subjects" || true)"
    n_fix="$(grep -cE '^fix(\([^)]*\))?!?:'   <<<"$subjects" || true)"
    n_break="$(grep -cE '^[a-z]+(\([^)]*\))?!:' <<<"$subjects" || true)"
    if git log --format='%b' "$range" | grep -qE 'BREAKING[ -]CHANGE'; then
        n_break=$((n_break + 1))
    fi

    if   [[ "$n_break" -gt 0 ]]; then level="major"
    elif [[ "$n_feat"  -gt 0 ]]; then level="minor"
    else                              level="patch"
    fi

    # Bump the chosen field of X.Y.Z; `10#` forces base-10 so a value
    # like `08` isn't mis-read as octal.
    IFS=. read -r vmaj vmin vpat <<<"$CUR_VER"
    case "$level" in
        major) suggested="$((10#$vmaj + 1)).0.0" ;;
        minor) suggested="$vmaj.$((10#$vmin + 1)).0" ;;
        patch) suggested="$vmaj.$vmin.$((10#$vpat + 1))" ;;
    esac

    say "$count commit(s) since $base_ref — $n_feat feat, $n_fix fix, $n_break breaking"
    say "suggested $level bump: $CUR_VER -> $suggested"
    read -r -p $'\nRelease version? (Enter to accept the suggestion) ['"$suggested"$'] ' ans
    NEW_VER="${ans:-$suggested}"
    NEW_VER="${NEW_VER#v}"
fi

[[ "$NEW_VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "version '$NEW_VER' is not a plain X.Y.Z semver"

TAG="v$NEW_VER"
TARBALL="tensaku-$TAG-x86_64.tar.gz"

[[ "$NEW_VER" != "$CUR_VER" ]] || die "$NEW_VER is already the current version"
[[ "$(printf '%s\n%s\n' "$CUR_VER" "$NEW_VER" | sort -V | tail -1)" == "$NEW_VER" ]] \
    || die "$NEW_VER is older than the current version $CUR_VER"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    die "tag $TAG already exists"
fi
say "bumping $CUR_VER -> $NEW_VER"

#--- bump versions + refresh Cargo.lock ----------------------------------------

# workspace.package.version
sed -i -E "/^\[workspace\.package\]/,/^version = / s/^version = .*/version = \"$NEW_VER\"/" Cargo.toml
# The tensaku_cli path dependency pins a version requirement that must
# track workspace.package.version, or cargo refuses to resolve the build.
sed -i -E "s|^(tensaku_cli = \{ path = \"cli\", version = )\"[^\"]*\"|\1\"$NEW_VER\"|" Cargo.toml

say "refreshing Cargo.lock"
cargo generate-lockfile

#--- fill in the NEXTRELEASE placeholder ---------------------------------------

say "substituting NEXTRELEASE -> $NEW_VER"
for f in cli/src/command_line.rs src/configuration.rs config.toml README.md; do
    sed -i "s/NEXTRELEASE/$NEW_VER/g" "$f"
done

#--- review + confirm ----------------------------------------------------------

git --no-pager diff
read -r -p $'\nProceed with commit, tag, and release? (Y/n) ' ans
if [[ "${ans,,}" == n* ]]; then
    die "aborted"
fi

#--- commit + tag + push -------------------------------------------------------

say "committing + tagging $TAG"
git commit -am "Release $TAG"
git tag -a "$TAG" -m "$TAG"

say "pushing to GitHub"
git push origin main
git push origin "$TAG"

#--- build the release tarball -------------------------------------------------

say "building the release tarball"
make package
[[ -f "$TARBALL" ]] || die "expected tarball '$TARBALL' was not produced by 'make package'"

#--- publish the GitHub Release ------------------------------------------------

say "publishing GitHub release $TAG"
if gh release view "$TAG" --repo "$GH_REPO" >/dev/null 2>&1; then
    gh release upload "$TAG" "$TARBALL" --repo "$GH_REPO" --clobber
else
    gh release create "$TAG" "$TARBALL" \
        --repo "$GH_REPO" \
        --title "$TAG" \
        --generate-notes
fi
rm -f "$TARBALL"

#--- update the AUR package ----------------------------------------------------

# The AUR package lives in its own repo (a separate clone). We only touch
# the version-dependent fields — pkgver, pkgrel, sha256sums — and the
# generated .SRCINFO; the rest of the PKGBUILD is maintained by hand.
# Best-effort: a failure here doesn't undo the release that already shipped.
AUR_DIR="${TENSAKU_AUR_DIR:-$HOME/Code/aur/tensaku}"
if [[ -d "$AUR_DIR/.git" ]]; then
    say "updating the AUR package at $AUR_DIR"
    if (
        set -e
        cd "$AUR_DIR"
        git pull --quiet --ff-only
        sha="$(curl -fsSL "https://github.com/$GH_REPO/archive/refs/tags/$TAG.tar.gz" \
            | sha256sum | cut -d' ' -f1)"
        [[ -n "$sha" ]]
        sed -i "s/^pkgver=.*/pkgver=$NEW_VER/" PKGBUILD
        sed -i "s/^pkgrel=.*/pkgrel=1/" PKGBUILD
        sed -i "s/^sha256sums=.*/sha256sums=('$sha')/" PKGBUILD
        makepkg --printsrcinfo > .SRCINFO
        git add PKGBUILD .SRCINFO
        git commit -q -m "Update to $NEW_VER"
        git push --quiet
    ); then
        say "AUR updated to $NEW_VER"
    else
        printf '\033[1;31m!! AUR update failed — update %s by hand\033[0m\n' "$AUR_DIR" >&2
    fi
else
    say "no AUR clone at $AUR_DIR — skipping (set TENSAKU_AUR_DIR or update it by hand)"
fi

#--- publish to crates.io ------------------------------------------------------

# tensaku_cli first: tensaku's path dependency on it must resolve
# against an already-published crate. `cargo publish` waits for the
# crate to land in the registry index before returning, so the second
# publish builds against it. CARGO_REGISTRY_TOKEN is set per-command,
# so the 1Password-sourced token reaches only these two processes.
say "publishing to crates.io"
CARGO_REGISTRY_TOKEN="$CRATESIO_TOKEN" cargo publish -p tensaku_cli
CARGO_REGISTRY_TOKEN="$CRATESIO_TOKEN" cargo publish -p tensaku

say "done — https://github.com/$GH_REPO/releases/tag/$TAG"
