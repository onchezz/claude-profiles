#!/bin/bash
# Build the cross-platform window (GPUI) and the command it drives, and put
# them side by side in dist/, which is where the window looks first.
#
#   scripts/build-gui.sh            build into dist/
#   scripts/build-gui.sh --install  also copy to ~/.local/bin (and, on Linux,
#                                   add a launcher to the applications menu)
#
# On Windows, run `cargo build --release --workspace` and keep
# claude-profiles.exe beside claude-profiles-gui.exe.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --workspace
mkdir -p dist
cp target/release/claude-profiles target/release/claude-profiles-gui dist/
echo "Built dist/claude-profiles-gui (with dist/claude-profiles beside it)."

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$HOME/.local/bin"
  cp dist/claude-profiles dist/claude-profiles-gui "$HOME/.local/bin/"
  if [[ "$(uname)" == "Linux" ]]; then
    mkdir -p "$HOME/.local/share/applications"
    sed "s|^Exec=.*|Exec=$HOME/.local/bin/claude-profiles-gui|" gui/claude-profiles.desktop \
      > "$HOME/.local/share/applications/claude-profiles.desktop"
  fi
  echo "Installed to ~/.local/bin. Run claude-profiles-gui."
fi
