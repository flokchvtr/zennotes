#!/bin/sh
# Update the Tauri build of ZenNotes: pull upstream, replay our patches,
# rebuild everything, install to /Applications/ZenNotes Tauri.app.
#
#   ./apps/desktop-tauri/update.sh            # update + rebuild + install
#   ./apps/desktop-tauri/update.sh --no-pull  # rebuild only (local changes)
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TAURI="$ROOT/apps/desktop-tauri"
APP_DEST="/Applications/ZenNotes Tauri.app"
cd "$ROOT"

if [ "${1:-}" != "--no-pull" ]; then
  echo "==> Fetching upstream and rebasing our patches"
  git fetch origin
  if ! git rebase origin/main; then
    git rebase --abort
    echo "!! Rebase conflict with upstream. Resolve manually:" >&2
    echo "   git rebase origin/main   # fix conflicts, then rerun with --no-pull" >&2
    exit 1
  fi
fi

echo "==> Installing npm dependencies"
npm install

echo "==> Typecheck + unit tests (fail fast before the long builds)"
npm run typecheck --workspace @zennotes/shared-domain --workspace @zennotes/web
npm run test:run --workspace @zennotes/shared-domain

echo "==> Building web bundle + Go server"
make server-build
mkdir -p "$TAURI/src-tauri/binaries"
cp apps/server/bin/zennotes-server \
  "$TAURI/src-tauri/binaries/zennotes-server-$(rustc -vV | sed -n 's/^host: //p')" \
  2>/dev/null || cp apps/server/bin/zennotes-server \
  "$TAURI/src-tauri/binaries/zennotes-server-aarch64-apple-darwin"

echo "==> Building MCP/CLI bundles"
npm run build --workspace @zennotes/desktop
mkdir -p "$TAURI/mcp-runtime"
cp -R apps/desktop/out/main/mcp.js apps/desktop/out/main/cli.js \
  apps/desktop/out/main/chunks "$TAURI/mcp-runtime/"

echo "==> Building TikZ runtime"
(cd "$TAURI/tikz-runtime" && npm install)
npx --yes esbuild apps/desktop/src/main/tikz.ts --bundle --platform=node \
  --format=esm --external:node-tikzjax --outfile="$TAURI/tikz-runtime/tikz-core.mjs"

echo "==> Building Tauri app"
(cd "$TAURI" && npx tauri build)

echo "==> Installing to $APP_DEST"
rm -rf "$APP_DEST"
cp -R "$TAURI/src-tauri/target/release/bundle/macos/ZenNotes.app" "$APP_DEST"

echo "==> Done: $(git log -1 --format='%h %s')"
