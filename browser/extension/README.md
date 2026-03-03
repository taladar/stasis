# Stasis Browser Extension (Scaffold)

Shared extension codebase for Firefox + Chromium-family browsers.

## What it does

- Detects active tab media elements (`video`/`audio`) and sends periodic keepalive pulses.
- Sends throttled keepalive messages to a native messaging host.
- Native host runs `stasis browser-activity`.
- On media stop/hidden/page leave, sends an explicit inactive pulse.

This keeps Stasis in the waiting-for-idle state without increasing inhibitor counters.

Behavior details:

- Media keepalive pulse throttle: 20s.
- Media keepalive only when tab is visible.
- No interaction-based pulses are emitted.
- Media-stop inactive pulse is debounced by 1s to avoid pause/play flapping.
- Chromium build also injects content script into already-open tabs on startup/install.

## Layout

- `src/content.js`: tab activity capture.
- `src/background.js`: native host bridge.
- `manifests/manifest.chromium.json`: MV3.
- `manifests/manifest.firefox.json`: MV2.
- `scripts/build.sh`: build both packages.

## Build

```bash
browser/extension/scripts/build.sh
```

Artifacts:

- `browser/extension/dist/stasis-browser-activity-chromium.zip`
- `browser/extension/dist/stasis-browser-activity-firefox.zip`
- `browser/extension/dist/stasis-browser-activity-firefox.xpi`

## Native host

Install the native host scaffold in `browser/native-host` before testing.
