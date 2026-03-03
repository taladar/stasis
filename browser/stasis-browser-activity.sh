#!/usr/bin/env sh
set -eu

# Minimal bridge helper for browser/native-host integrations.
# Emits one browser activity pulse to the running Stasis daemon.
exec stasis browser-activity
