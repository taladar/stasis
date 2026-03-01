# Changelog
All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [1.0.1] - TBD

### Changed

- `media.rs`: replaced `sh -lc pactl` invocation with a direct `pactl` call, removing the unnecessary shell wrapper.

- **media: complete overhaul of sink-input and source-output detection**
  - Removed `pactl list sinks` RUNNING gate — sink state persists after leaving a Discord call, causing false positives that held inhibitors open indefinitely.
  - Added `pactl list source-outputs` parsing. Any active (uncorked) source-output counts as a call inhibitor (`call` bucket), independent of sink-input state. This correctly reflects mic capture as an active session signal.
  - Firefox sink-inputs are now deduped by `media.name` (tab title) rather than `object.serial`, preventing PipeWire from double-counting the same tab via multiple sink-input blocks.
  - Firefox sink-inputs whose `media.name` contains `"discord"` are always suppressed (covers `"• Discord | General | …"` tab titles).
  - Any browser PID found in `capturing_pids` (has an active source-output) has its generic-named sink-inputs suppressed. Real media titles (YouTube, etc.) are not affected and always pass through.
  - Chromium/Vivaldi sink-inputs with generic `media.name` values (`"Playback"`, `"AudioStream"`, etc.) are suppressed when the PID is actively capturing — correctly handling Vivaldi's Discord call audio zombie without blocking legitimate YouTube playback.
  - `playing_streams_total` and `playing_streams_chromium` heuristic counters are now incremented **after** filtering, ensuring the chromium single-stream heuristic fires correctly even when a filtered-out Firefox Discord tab is simultaneously uncorked.
  - Reduced `chromium_single_grace_ms` from 5 000 ms to 1 500 ms for snappier post-call cleanup.
  - Replaced large closure-with-18-parameters pattern with `macro_rules! flush!()` in both sink-input and source-output parsers, improving readability and eliminating borrow-checker friction.

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
- Fixed inhibitor count staying permanently elevated after leaving a Discord call in any browser.
- Fixed Firefox counting one playing tab as two due to PipeWire creating duplicate sink-input blocks.
- Fixed Chromium/Vivaldi Discord zombie stream holding `local=1` after a call ends.
- Fixed chromium single-stream heuristic never firing when a filtered Firefox Discord tab was simultaneously uncorked (inflating `streams_total` and blocking the heuristic).

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
