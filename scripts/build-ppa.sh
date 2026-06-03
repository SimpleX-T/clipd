#!/usr/bin/env bash
# scripts/build-ppa.sh — produce a Launchpad-uploadable source package.
#
# Usage:
#   scripts/build-ppa.sh                       # build for whatever series
#                                              # is already in debian/changelog
#   scripts/build-ppa.sh noble                 # re-stamp changelog top entry
#                                              # for `noble` and build
#   scripts/build-ppa.sh noble questing resolute
#                                              # build a separate source
#                                              # package per series
#
# After the build:
#   dput ppa:simplex-t/clipd ../clipd_<version>~<series>1_source.changes
#
# Launchpad builders run sbuild in a network-less chroot. Cargo crates
# cannot be fetched at build time, so we vendor them into ./vendor/
# locally and ship that directory inside the source tarball. The local
# .cargo/config.toml redirects cargo's `crates-io` source at the vendor
# tree.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Optional: pass KEY=<fingerprint-or-uid> to override the signing key
# (e.g. when your Launchpad-registered GPG key has a different UID from
# the changelog's Maintainer line). KEY="" disables signing entirely —
# useful for local sanity-check builds, but the resulting source package
# is NOT uploadable to Launchpad.
SIGN_ARGS=()
if [ "${KEY+set}" = "set" ]; then
    if [ -z "${KEY}" ]; then
        SIGN_ARGS=(-us -uc)
        echo "==> KEY is empty — building unsigned (local test only)"
    else
        SIGN_ARGS=("-k${KEY}")
        echo "==> signing with key: $KEY"
    fi
fi

# 1. Vendor cargo deps (idempotent — cargo skips work if vendor/ is fresh).
echo "==> cargo vendor (this can take a minute on first run)"
mkdir -p .cargo
# cargo vendor prints the [source.*] stanza it wants on stdout — we
# pipe that into .cargo/config.toml so the Launchpad builder uses it.
cargo vendor --locked > .cargo/config.toml

# 2. Sanity-check that debian/ is in shape.
if ! command -v dpkg-buildpackage >/dev/null; then
    echo "error: dpkg-buildpackage not installed. apt install devscripts dpkg-dev"
    exit 1
fi
if ! command -v dch >/dev/null; then
    echo "error: dch not installed. apt install devscripts"
    exit 1
fi

# Argument list = explicit series targets. If empty, just build the
# tarball for whatever's already in debian/changelog (which lets the
# script be useful for local sbuild testing too).
SERIES_LIST=("$@")
if [ "${#SERIES_LIST[@]}" -eq 0 ]; then
    echo "==> dpkg-buildpackage -S (using existing debian/changelog)"
    dpkg-buildpackage -S -sa -d "${SIGN_ARGS[@]}"
    echo
    echo "Built. Upload with:"
    echo "   dput ppa:simplex-t/clipd ../clipd_$(dpkg-parsechangelog -S Version)_source.changes"
    exit 0
fi

# Multi-series mode: produce one source package per series. We
# round-trip through dch which writes a fresh changelog entry stamped
# with the target series. Versions are <upstream>~<series>1 with no
# Debian revision — format 3.0 (native) doesn't allow one.
UPSTREAM_VERSION="$(dpkg-parsechangelog -S Version | sed 's/~.*//')"

for SERIES in "${SERIES_LIST[@]}"; do
    echo "==> series: $SERIES"
    VER="${UPSTREAM_VERSION}~${SERIES}1"
    # --force-distribution: dch otherwise picks the current host's series.
    DEBEMAIL="ntmark2004@gmail.com" DEBFULLNAME="SimpleX-T" \
        dch --force-distribution --newversion "$VER" --distribution "$SERIES" \
            "Backport for $SERIES."
    dpkg-buildpackage -S -sa -d "${SIGN_ARGS[@]}"
    echo "   built ../clipd_${VER}_source.changes"
done

echo
echo "All series built. Upload with:"
for SERIES in "${SERIES_LIST[@]}"; do
    VER="${UPSTREAM_VERSION}~${SERIES}1"
    echo "   dput ppa:simplex-t/clipd ../clipd_${VER}_source.changes"
done
