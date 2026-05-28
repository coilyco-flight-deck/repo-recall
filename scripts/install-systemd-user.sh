#!/usr/bin/env bash
# Install repo-recall as a per-user systemd service (Linux / WSL surface).
# Idempotent; needs a working `cc` (see kai-wsl-env skill for the zig shim).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="${HOME}/.local/bin"
unit_dir="${HOME}/.config/systemd/user"
unit_name="repo-recall.service"

if ! command -v cc >/dev/null 2>&1; then
  echo "error: no C compiler 'cc' on PATH; cargo build will fail." >&2
  echo "       on WSL/Focal, symlink cc -> zig shim (see kai-wsl-env skill)." >&2
  exit 1
fi

echo "==> building release binary"
cargo build --release --manifest-path "${repo_root}/Cargo.toml"

echo "==> installing binary to ${bin_dir}/repo-recall"
mkdir -p "${bin_dir}"
install -m 0755 "${repo_root}/target/release/repo-recall" "${bin_dir}/repo-recall"

echo "==> installing unit to ${unit_dir}/${unit_name}"
mkdir -p "${unit_dir}"
install -m 0644 "${repo_root}/scripts/${unit_name}" "${unit_dir}/${unit_name}"

echo "==> enabling lingering so the service starts at boot without a login"
loginctl enable-linger "$(id -un)" 2>/dev/null || \
  echo "    (could not enable-linger without privileges; may already be set)"

echo "==> reloading + (re)starting service"
systemctl --user daemon-reload
systemctl --user enable --now "${unit_name}"
systemctl --user restart "${unit_name}"

echo "==> status"
systemctl --user --no-pager status "${unit_name}" | head -8
