#!/usr/bin/env bash
set -euo pipefail

cargo xwin build --release --target x86_64-pc-windows-msvc --manifest-path legendary_manager/Cargo.toml

addons_dir="${GW2_ADDONS_DIR:-/run/media/tulio/ssd/Games/Guild Wars 2/addons}"
dll_name="legendary_manager.dll"
source_path="legendary_manager/target/x86_64-pc-windows-msvc/release/${dll_name}"

if [[ ! -d "$addons_dir" ]]; then
  echo "GW2 addons directory not found: $addons_dir" >&2
  echo "Set GW2_ADDONS_DIR to the directory that should receive ${dll_name}." >&2
  exit 1
fi

if [[ ! -f "$source_path" ]]; then
  echo "Build output not found: $source_path" >&2
  exit 1
fi

cp "$source_path" "$addons_dir/"

echo "Build and copy complete: $addons_dir/${dll_name}"
