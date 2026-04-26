#!/usr/bin/env bash
set -euo pipefail

cargo xwin build --release --target x86_64-pc-windows-msvc

addons_dir="${GW2_ADDONS_DIR:-/mnt/ssd/Games/Guild Wars 2/addons}"

if [[ ! -d "$addons_dir" ]]; then
    echo "GW2 addons directory not found: $addons_dir" >&2
    echo "Set GW2_ADDONS_DIR to the directory that should receive chat_link_generator.dll." >&2
    exit 1
fi

cp target/x86_64-pc-windows-msvc/release/chat_link_generator.dll "$addons_dir/"

echo "Build and copy complete: $addons_dir/chat_link_generator.dll"
