#!/usr/bin/env bash
set -euo pipefail

cargo xwin build --release --target x86_64-pc-windows-msvc

cp target/x86_64-pc-windows-msvc/release/chat_link_generator.dll "/media/tulio/ExtremeSSD/GW2__133/drive_c/Program Files/Guild Wars 2/addons/"

echo "Build and copy complete."
