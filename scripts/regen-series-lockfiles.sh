#!/usr/bin/env bash
# scripts/regen-series-lockfiles.sh — generate per-series Cargo.lock files.
#
# Different Ubuntu series ship different rustc versions, and some of our
# deps require newer rustc than older series have. Cargo's MSRV-aware
# resolver (cargo >= 1.84, `incompatible-rust-versions = "fallback"`)
# picks the newest crate versions whose `rust-version` field is <= a
# given floor. We run it once per series and stash the result under
# debian/lockfiles/, so the PPA build can swap in the right lockfile per
# target series.
#
# Run from repo root, with a network connection:
#   scripts/regen-series-lockfiles.sh
#
# Re-run when any workspace dep is added/bumped — the per-series
# lockfiles need refreshing to pick up new transitive versions.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Series → rustup toolchain. Numbers come from the Launchpad build
# logs: questing ships rustc 1.85.1. resolute uses the canonical
# Cargo.lock (latest deps) — no entry here.
#
# Noble (24.04, rustc 1.75) is INTENTIONALLY ABSENT. The crates-io
# index itself now uses Cargo features only stabilized in cargo 1.85,
# so cargo 1.75 fails to even parse the registry — no per-crate pinning
# can rescue noble. Noble users install from source after `rustup
# install stable`. See README "From source" section.
#
# We MUST run cargo under each target toolchain. Cargo's MSRV-aware
# resolver compares dep `rust-version` against max(workspace.rust-version,
# locally-installed rustc) — so running under our local stable (1.92)
# would let through deps that need 1.86 even with rust-version="1.85".
# Running `cargo +1.85.0` makes the locally-installed rustc the same
# as the floor, forcing the resolver to pick MSRV-1.85-compatible deps.
SERIES=(questing)
TOOLCHAIN=(1.85.0)
MSRV=(1.85)

# Verify both toolchains are installed before we touch anything.
for t in "${TOOLCHAIN[@]}"; do
    if ! rustup toolchain list | grep -q "^${t}-"; then
        echo "error: rustup toolchain ${t} not installed."
        echo "       rustup toolchain install ${t} --profile minimal"
        exit 1
    fi
done

mkdir -p debian/lockfiles

# Stash the canonical state. We'll restore on exit even if cargo errors.
cp Cargo.toml Cargo.toml.regen-backup
cp Cargo.lock Cargo.lock.regen-backup
HAD_CONFIG=0
if [ -f .cargo/config.toml ]; then
    HAD_CONFIG=1
    cp .cargo/config.toml .cargo/config.toml.regen-backup
fi

restore() {
    mv Cargo.toml.regen-backup Cargo.toml 2>/dev/null || true
    mv Cargo.lock.regen-backup Cargo.lock 2>/dev/null || true
    if [ "$HAD_CONFIG" = "1" ]; then
        mv .cargo/config.toml.regen-backup .cargo/config.toml 2>/dev/null || true
    else
        rm -f .cargo/config.toml
    fi
}
trap restore EXIT

# Tell cargo's resolver to fall back to MSRV-compatible versions.
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
# Ephemeral — written by scripts/regen-series-lockfiles.sh. The trap
# in that script restores any prior .cargo/config.toml on exit.
[resolver]
incompatible-rust-versions = "fallback"
EOF

for i in "${!SERIES[@]}"; do
    s="${SERIES[$i]}"
    tc="${TOOLCHAIN[$i]}"
    msrv="${MSRV[$i]}"
    echo
    echo "==> $s (toolchain $tc, rust-version $msrv)"

    # Pin the workspace rust-version so the resolver knows the floor.
    sed -i "s/^rust-version = .*/rust-version = \"$msrv\"/" Cargo.toml

    # Force a fresh resolution from scratch — no stale Cargo.lock.
    rm -f Cargo.lock
    if ! cargo "+${tc}" generate-lockfile 2>&1 | tail -25; then
        echo "   FAILED: cargo +$tc could not resolve a dep set"
        echo "   compatible with rustc $msrv. This series can't be"
        echo "   supported without manually intervening (swap a dep,"
        echo "   or drop it)."
        exit 1
    fi

    # Downgrade to v3 format — noble's cargo 1.75 doesn't read v4.
    sed -i 's/^version = 4$/version = 3/' Cargo.lock

    cp Cargo.lock "debian/lockfiles/${s}.lock"
    echo "   wrote debian/lockfiles/${s}.lock ($(wc -l <"debian/lockfiles/${s}.lock") lines)"
done

echo
echo "All series lockfiles generated:"
ls -lh debian/lockfiles/

echo
echo "Next: commit debian/lockfiles/ and run scripts/build-ppa.sh"
