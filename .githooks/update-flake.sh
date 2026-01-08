#!/usr/bin/env bash
set -e

PACKAGE_NAME="$1"

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <package_name>" >&2
  exit 1
fi

if git diff --cached --name-only | grep -qE "Cargo\.(lock|toml)"; then
  VERSION=$(yj -t < Cargo.toml | jq -r ".package.version")
  nix-update $PACKAGE_NAME --version=$VERSION --flake
  git add flake.nix
fi
