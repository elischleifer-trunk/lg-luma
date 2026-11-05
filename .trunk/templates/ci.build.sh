#!/usr/bin/env bash
# Builds the Trunk runners image for .github/workflows/ci.yml (template: ci.yaml).
# Trunk sources this file into one bash shell during the image build. Do not use `exit 0`.
# Each section reads its version from the same file that the workflow reads, and is idempotent.
set -euo pipefail

REPO_DIR=/workspace/lg-luma
WORKFLOW=.github/workflows/ci.yml
# These come from the template env (ci.yaml).
: "${RUSTUP_HOME:?RUSTUP_HOME must be set in ci.yaml env}"
: "${CARGO_HOME:?CARGO_HOME must be set in ci.yaml env}"
: "${npm_config_store_dir:?npm_config_store_dir must be set in ci.yaml env}"
cd "$REPO_DIR"

# --- Node.js: the base image already has Node 24. Only verify the major version. ---
node_wanted=$(sed -nE "s/^[[:space:]]*node-version:[[:space:]]*'?\"?([0-9]+)'?\"?[[:space:]]*$/\1/p" "$WORKFLOW" | sort -u)
if [[ $(wc -l <<<"$node_wanted") -ne 1 || -z $node_wanted ]]; then
  echo "Expected exactly one node-version in $WORKFLOW, found: ${node_wanted:-none}" >&2
  exit 1
fi
node_have=$(node -p 'process.versions.node.split(".")[0]')
if [[ $node_have != "$node_wanted" ]]; then
  echo "Base image has Node $node_have; $WORKFLOW wants Node $node_wanted." >&2
  exit 1
fi

# --- System packages: the union of the apt installs in the prepare, smoke and package jobs. ---
apt_packages=(libsdl2-dev libsdl2-2.0-0 pkg-config woff2 python3-fonttools libgl1-mesa-dri xvfb xauth)
apt_missing=()
for package in "${apt_packages[@]}"; do
  if ! dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed'; then
    apt_missing+=("$package")
  fi
done
if ((${#apt_missing[@]})); then
  apt_options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
  sudo apt-get "${apt_options[@]}" update
  sudo DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install -y --no-install-recommends "${apt_missing[@]}"
fi

# --- pnpm: the version in package.json#packageManager (pnpm/action-setup reads the same field). ---
pnpm_wanted=$(node -p 'require("./package.json").packageManager.replace(/^pnpm@/, "")')
if [[ $(pnpm --version 2>/dev/null || true) != "$pnpm_wanted" ]]; then
  sudo npm install --global "pnpm@$pnpm_wanted"
fi

# --- Rust: the version pinned by dtolnay/rust-toolchain@<version> in the workflow. ---
rust_wanted=$(grep -oE 'dtolnay/rust-toolchain@[0-9]+\.[0-9]+\.[0-9]+' "$WORKFLOW" | cut -d@ -f2 | sort -u)
if [[ $(wc -l <<<"$rust_wanted") -ne 1 || -z $rust_wanted ]]; then
  echo "Expected exactly one dtolnay/rust-toolchain version in $WORKFLOW, found: ${rust_wanted:-none}" >&2
  exit 1
fi
sudo mkdir -p "$RUSTUP_HOME" "$CARGO_HOME"
sudo chown -R "$(id -u):$(id -g)" "$RUSTUP_HOME" "$CARGO_HOME"
if [[ ! -x $CARGO_HOME/bin/rustup ]]; then
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 3 https://sh.rustup.rs |
    sh -s -- -y --no-modify-path --profile minimal --default-toolchain none
fi
"$CARGO_HOME/bin/rustup" toolchain install "$rust_wanted" --profile minimal \
  --component rustfmt,clippy --target armv7-unknown-linux-gnueabi --no-self-update
"$CARGO_HOME/bin/rustup" default "$rust_wanted"
# Put the rustup proxies on the default PATH of the build shell and of each job.
for tool in "$CARGO_HOME"/bin/*; do
  sudo ln -sf "$tool" "/usr/local/bin/$(basename "$tool")"
done

# --- JavaScript dependencies: node_modules in the clone, from the lockfile. ---
sudo mkdir -p "$npm_config_store_dir"
sudo chown -R "$(id -u):$(id -g)" "$npm_config_store_dir"
pnpm install --frozen-lockfile

# --- Rust dependencies and warm builds: the same cargo commands as CI. ---
# Swatinem/rust-cache sets CARGO_INCREMENTAL=0 in the jobs; match it so the builds agree.
export CARGO_INCREMENTAL=0
cargo fetch --locked
# prepare job: pnpm check, pnpm test, and the shared desktop preview.
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features --no-run
cargo test --locked -p luma-tv-helper --no-run
cargo build --locked --package luma --no-default-features
# prepare job: pnpm assets compiles the SVG geometry tool into its own target directory.
CARGO_TARGET_DIR="$REPO_DIR/.build/vector-assets-target" \
  cargo build --locked --release --manifest-path "$REPO_DIR/tools/vector-assets/Cargo.toml"
# package job: the webOS SDK (.build/toolchain), SDL-webOS (.build/sdl-webos) and the armv7 build.
pnpm sdk
WEBOS_NDK="$REPO_DIR/.build/toolchain/arm-webos-linux-gnueabi_sdk-buildroot" pnpm build

# --- Hand everything to the job user (UID 1001), then refresh the git index last. ---
sudo chown -R 1001:1001 "$REPO_DIR" "$RUSTUP_HOME" "$CARGO_HOME" "$npm_config_store_dir"
sudo git -c safe.directory="$REPO_DIR" -C "$REPO_DIR" update-index --refresh >/dev/null || true
sudo chown 1001:1001 "$REPO_DIR/.git/index"
