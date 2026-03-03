#!/usr/bin/env sh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
HOST_SCRIPT="$ROOT_DIR/stasis_native_host.py"
TEMPLATES_DIR="$ROOT_DIR/manifests"
HOST_NAME="io.github.saltnpepper97.stasis"

CHROMIUM_ORIGIN=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --chromium-origin)
      if [ "$#" -lt 2 ]; then
        echo "missing value for --chromium-origin" >&2
        exit 1
      fi
      CHROMIUM_ORIGIN="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      echo "usage: $0 [--chromium-origin chrome-extension://<id>/]" >&2
      exit 1
      ;;
  esac
done

normalize_chromium_origin() {
  v="$1"
  if [ -z "$v" ]; then
    printf '%s' ""
    return
  fi

  case "$v" in
    chrome-extension://*)
      ;;
    *)
      v="chrome-extension://$v"
      ;;
  esac

  case "$v" in
    */) ;;
    *) v="$v/" ;;
  esac

  printf '%s' "$v"
}

CHROMIUM_ORIGIN="$(normalize_chromium_origin "$CHROMIUM_ORIGIN")"

mkdir -p "$HOME/.mozilla/native-messaging-hosts"
chmod +x "$HOST_SCRIPT"

escape_path() {
  printf '%s' "$1" | sed 's/[\\&]/\\&/g'
}

HOST_PATH_ESCAPED="$(escape_path "$HOST_SCRIPT")"

sed "s|__HOST_PATH__|$HOST_PATH_ESCAPED|g" \
  "$TEMPLATES_DIR/$HOST_NAME.firefox.json.template" \
  > "$HOME/.mozilla/native-messaging-hosts/$HOST_NAME.json"

echo "installed firefox native host manifest"

if [ -n "$CHROMIUM_ORIGIN" ]; then
  ORIGIN_ESCAPED="$(printf '%s' "$CHROMIUM_ORIGIN" | sed 's/[\\&]/\\&/g')"

  for base in \
    "$HOME/.config/chromium/NativeMessagingHosts" \
    "$HOME/.config/google-chrome/NativeMessagingHosts" \
    "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts" \
    "$HOME/.config/vivaldi/NativeMessagingHosts"
  do
    mkdir -p "$base"
    sed \
      -e "s|__HOST_PATH__|$HOST_PATH_ESCAPED|g" \
      -e "s|__CHROMIUM_ORIGIN__|$ORIGIN_ESCAPED|g" \
      "$TEMPLATES_DIR/$HOST_NAME.chromium.json.template" \
      > "$base/$HOST_NAME.json"
  done

  echo "installed chromium native host manifests for origin: $CHROMIUM_ORIGIN"
else
  echo "skipped chromium native host manifests (no --chromium-origin provided)"
fi

echo "done"
