# Stasis Native Messaging Host (Scaffold)

Native host that receives extension activity events and runs:

```bash
stasis browser-activity
```

## Files

- `stasis_native_host.py`: stdio native host.
- `manifests/*.template`: host manifest templates.
- `scripts/install.sh`: installs manifests into user config dirs.

## Install

Firefox only:

```bash
browser/native-host/scripts/install.sh
```

Firefox + Chromium-origin:

```bash
browser/native-host/scripts/install.sh --chromium-origin chrome-extension://<EXTENSION_ID>/
```

For Chromium, extension ID is required in `allowed_origins`.
You can also pass just the raw extension ID; installer normalizes it.
