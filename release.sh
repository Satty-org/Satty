#!/usr/bin/env bash
#
# release.sh — cut a new tensaku release.
#
# Run from a clean working tree on `main`:
#
#   ./release.sh 0.22.0
#
# The script bumps the version, refreshes Cargo.lock, fills in the
# NEXTRELEASE placeholder, commits + tags, pushes to GitHub, builds
# the release tarball, publishes the GitHub Release, and updates the
# AUR package. crates.io stays manual — the script prints a reminder.

set -euo pipefail

say() { printf '\n\033[1;33m++ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m!! %s\033[0m\n' "$*" >&2; exit 1; }

#--- arguments -----------------------------------------------------------------

NEW_VER="${1:-}"
NEW_VER="${NEW_VER#v}"
[[ -n "$NEW_VER" ]] || die "usage: $0 <new-version>   e.g. $0 0.22.0"
[[ "$NEW_VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "version '$NEW_VER' is not a plain X.Y.Z semver"

TAG="v$NEW_VER"
GH_REPO="jondkinney/tensaku"
TARBALL="tensaku-$TAG-x86_64.tar.gz"

cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

#--- preconditions -------------------------------------------------------------

say "checking preconditions"

for tool in gh make cargo; do
    command -v "$tool" >/dev/null || die "$tool is not installed"
done
gh auth status >/dev/null 2>&1 || die "gh is not authenticated — run 'gh auth login'"

[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] \
    || die "not on the main branch — check out main first"
[[ -z "$(git status --porcelain)" ]] \
    || die "working tree is not clean — commit or stash your changes first"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    die "tag $TAG already exists"
fi

CUR_VER="$(awk -F'"' '/^version = "/{print $2; exit}' Cargo.toml)"
[[ "$NEW_VER" != "$CUR_VER" ]] || die "$NEW_VER is already the current version"
[[ "$(printf '%s\n%s\n' "$CUR_VER" "$NEW_VER" | sort -V | tail -1)" == "$NEW_VER" ]] \
    || die "$NEW_VER is older than the current version $CUR_VER"
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

say "done — https://github.com/$GH_REPO/releases/tag/$TAG"

#--- crates.io is a manual step ------------------------------------------------

printf '\n\033[1;33m++ crates.io is NOT automated — publish it by hand:\033[0m\n'
printf '   cargo publish -p tensaku_cli && cargo publish -p tensaku\n'
