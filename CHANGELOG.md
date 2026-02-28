# Changelog
All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
---
## [TBD] - TBD

### Changed
- `media.rs`: replaced `sh -lc pactl` invocation with a direct `pactl` call, removing the unnecessary shell wrapper.
- **Firefox media detection**:
  - Restored per-tab counting by switching deduplication from process ID to `object.serial`, allowing multiple uncorked Firefox sink-inputs to increment the inhibitor count correctly again.
  - Discord audio in Firefox is now ignored to prevent browser-based Discord tabs or calls from indefinitely blocking media inhibition. Users should manually pause/inhibit Stasis while in a Discord call if desired.

### Notes
- Chromium/Vivaldi media detection remains imperfect:
  - YouTube playback may continue to inhibit until the tab or video is fully closed in some cases.
  - Browser-based Discord behavior under Chromium is currently unreliable.
  - Further refinement is planned.

---
## [1.0.0] - 2026-02-26
### Highlights
- Complete event-driven rewrite
- Improved memory handling and streamlined internals
- Services moved out of `core/`
- Eventline refactor and cleanup
- Built-in configuration migrator
- New logo and visual identity
- Logs moved to XDG-compliant state directory
### Added
- **Event-driven architecture** — timers, system signals, lid events, loginctl events, IPC pauses, and media state changes are now coordinated through a structured event system, replacing sequential and implicit flow. Results in more predictable state transitions, cleaner internal boundaries, reduced memory overhead, improved long-running stability, and a more extensible foundation for future features.
- **Built-in config migrator** — on first launch of 1.0.0, Stasis automatically migrates existing Rune configurations to the latest schema. Most users will not need to manually edit their configuration after upgrading.
### Changed
- **Media monitoring** — the browser-based media bridge has been removed. Stasis now relies exclusively on `pactl` for media detection. `pipewire-pulse` or `pulseaudio` must be installed and available.
- **`use_loginctl` → `enable_loginctl`** — renamed and moved to the top level under `default:` in the Rune configuration. No longer defined inside a nested block.
  ```rune
  default:
    enable_loginctl true
  end
  ```
- **Log directory** — logs now live in `~/.local/state/stasis/` (previously `~/.cache/stasis/`), aligning with the XDG Base Directory Specification.
- **Services** moved out of `core/`.
- **Eventline** received structural updates and cleanup.
### Fixed
- Resolved memory issues related to event handling
- Eliminated instability from the legacy media bridge
- Improved long-running session stability
- Streamlined internal code paths and reduced state drift
