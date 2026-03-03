#!/usr/bin/env sh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
TMP_DIR="$ROOT_DIR/.build-tmp"

rm -rf "$TMP_DIR"
mkdir -p "$DIST_DIR" "$TMP_DIR"

zip_dir() {
  src_dir="$1"
  out_file="$2"

  if command -v zip >/dev/null 2>&1; then
    (
      cd "$src_dir"
      zip -qr "$out_file" .
    )
    return
  fi

  if ! command -v python3 >/dev/null 2>&1; then
    echo "error: neither 'zip' nor 'python3' is available" >&2
    exit 1
  fi

  SRC_DIR="$src_dir" OUT_FILE="$out_file" python3 - <<'PY'
import os
import pathlib
import zipfile

src = pathlib.Path(os.environ["SRC_DIR"])
out = pathlib.Path(os.environ["OUT_FILE"])
out.parent.mkdir(parents=True, exist_ok=True)

with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as zf:
    for path in src.rglob("*"):
        if path.is_file():
            zf.write(path, path.relative_to(src))
PY
}

build_target() {
  target="$1"
  stage="$TMP_DIR/$target"

  rm -rf "$stage"
  mkdir -p "$stage"

  cp -r "$ROOT_DIR/src" "$stage/src"
  cp "$ROOT_DIR/manifests/manifest.$target.json" "$stage/manifest.json"

  archive="$DIST_DIR/stasis-browser-activity-$target.zip"
  zip_dir "$stage" "$archive"

  if [ "$target" = "firefox" ]; then
    cp "$archive" "$DIST_DIR/stasis-browser-activity-firefox.xpi"
  fi

  echo "built: $archive"
}

build_target chromium
build_target firefox

echo "done"
