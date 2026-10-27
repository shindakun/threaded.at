#!/bin/sh
# Builds hydrant at the pinned commit, as the deploy user on the droplet.
# usage: build-hydrant.sh [SOURCE_DIR]   (default /opt/hydrant/src)
#
# The lockfile updates below fix 13 of the 17 advisories against hydrant's
# lockfile at this commit and replace its yanked releases; docs/0006-m4.md
# lists them, and the four advisories left.
# Only these packages move: a plain `cargo update` would also move hydrant's
# git dependencies, some named by branch. `--locked` then refuses to build if
# anything else would change Cargo.lock.
set -eu

REPO=https://tangled.org/did:plc:dfl62fgb7wtjj3fcbb72naae/hydrant
COMMIT=bbd9b93acbe3463af42face09a26a2ffc025d6fd
SRC=${1:-/opt/hydrant/src}

if [ ! -d "$SRC/.git" ]; then
  git clone "$REPO" "$SRC"
fi
cd "$SRC"
git fetch --quiet origin
# --force drops the previous run's Cargo.lock changes and puts
# .cargo/config.toml back, so every run starts from the pinned commit.
git checkout --quiet --force --detach "$COMMIT"
test "$(git rev-parse HEAD)" = "$COMMIT"

# hydrant's config links with clang and the wild linker, which the droplet
# does not have; its Dockerfile leaves the file out too.
mv .cargo/config.toml .cargo/config.toml.off

# Each line is PACKAGE=VERSION; PACKAGE names the old version where the lock
# holds two of one crate. The last three replace yanked releases.
for spec in \
  h2=0.4.20 \
  rustls=0.23.46 \
  tokio=1.54.1 \
  quinn-proto=0.11.19 \
  crossbeam-epoch=0.9.21 \
  scc=3.8.8 \
  anyhow=1.0.104 \
  ctutils=0.4.6 \
  zstd-sys=2.1.2+zstd.1.5.7 \
  async-compression=0.4.51 \
  compression-codecs=0.4.46 \
  chacha20=0.10.2 \
  spin@0.9.8=0.9.9 \
  spin@0.10.0=0.10.1
do
  cargo update --quiet -p "${spec%=*}" --precise "${spec#*=}"
done

cargo build --release --locked
echo "built $SRC/target/release/hydrant at $COMMIT"
