#!/bin/bash
# Build script for Mini App examples

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Building Mini App examples..."

# Build clicker game
echo "  Building clicker.xdc..."
cd "$SCRIPT_DIR/clicker"
zip -r ../clicker.xdc index.html manifest.toml icon.svg
echo "  ✓ clicker.xdc created"

echo ""
echo "All Mini Apps built successfully!"
echo "Output files are in: $SCRIPT_DIR/"
ls -la "$SCRIPT_DIR"/*.xdc