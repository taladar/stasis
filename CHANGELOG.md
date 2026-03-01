# Changelog
All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [TBD] - TBD

### Changed

- `media.rs`: replaced `sh -lc pactl` invocation with a direct `pactl` call, removing the unnecessary shell wrapper.

- **media: restore Firefox per-tab counting; refine Chromium handling; ignore Discord audio**
  - Use `object.serial` as the Firefox dedup key so multiple uncorked sink-inputs count per tab again.
  - Drop Firefox sink-inputs whose `media.name` looks like Discord to prevent “Discord tab/call” from inhibiting indefinitely.
  - Small refinements to Chromium-based browser handling to better ignore Discord-related audio streams.
  - Note: Discord calls are now expected to use manual pause/inhibit instead.
  - Chromium/Vivaldi detection now behaves correctly, though media state changes (e.g. YouTube stopping) may resolve slightly slower than Firefox due to inherent browser/pipewire behavior.

- **Logging noise reduction (IPC & event scopes)**
  - Gated `eventline::scope!("event")` behind `--verbose`, eliminating `done: event#N` spam during normal operation.
  - Gated per-request IPC scopes behind `--verbose`, preventing excessive log output caused by frequent `stasis info --json` polling (e.g. Waybar modules).
  - Normal daemon mode now produces clean, stable logs while preserving full tracing in verbose mode.

- **Bootstrap configuration defaults**
  - Updated generated default configs to better reflect current suspend/lock semantics.
  - Clarified `pre_suspend_command` usage in generated templates and documentation.
  - Desktop and laptop templates now more clearly separate lock-step behavior from suspend behavior.

- **Suspend semantics clarification**
  - `pre_suspend_command` is now documented as intended for use with backgrounded (`daemonize`) suspend commands.
  - Users with a `lock_screen:` plan step no longer need `pre_suspend_command` in most cases.
  - Documentation updated to prevent misconfiguration where suspend races ahead of the locker.

- **IPC stability polish**
  - Reduced log overhead during frequent `info` calls.
  - Improved daemon cleanliness under heavy polling scenarios.

### Fixed

- Eliminated excessive `done: event#…` log lines during normal operation.
- Prevented Waybar polling from flooding daemon logs.
- Reduced log churn under steady-state idle operation.

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
