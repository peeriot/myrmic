#!/bin/bash
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "Usage: $0 <cell-name>"
    echo "Example: $0 room"
    exit 1
fi

CELL_NAME="$1"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE_DIR="$SCRIPT_DIR/workspace/$CELL_NAME"

if [ -d "$WORKSPACE_DIR" ]; then
    echo "Error: workspace already exists at $WORKSPACE_DIR"
    exit 1
fi

# Compute the relative path from the workspace to the sdk/ directory in the
# main repository. The workspace lives at tutorials/cell-tools/workspace/<name>/
# and sdk/ lives at the repo root next to swarm/.
WASM_ROOT="../../../sdk"

echo "Creating workspace for cell '$CELL_NAME'..."

# Copy the template
cp -r "$SCRIPT_DIR/template" "$WORKSPACE_DIR"

# Rename the placeholder crate directory
mv "$WORKSPACE_DIR/CELL_NAME" "$WORKSPACE_DIR/$CELL_NAME"

# Replace placeholders in all files
if [[ "$OSTYPE" == "darwin"* ]]; then
    SED_INPLACE=(sed -i '')
else
    SED_INPLACE=(sed -i)
fi

find "$WORKSPACE_DIR" -type f \( -name '*.toml' -o -name '*.rs' \) | while read -r file; do
    "${SED_INPLACE[@]}" "s|CELL_NAME|$CELL_NAME|g" "$file"
    "${SED_INPLACE[@]}" "s|WASM_ROOT|$WASM_ROOT|g" "$file"
done

echo "Workspace created at: $WORKSPACE_DIR"
echo ""
echo "Next steps:"
echo "  1. Edit $WORKSPACE_DIR/$CELL_NAME/src/lib.rs"
echo "  2. Build with: workspace/cell-tools build $WORKSPACE_DIR/$CELL_NAME $CELL_NAME"
