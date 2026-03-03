<p align="center">
  <img src="assets/stasis.png" alt="Stasis Logo" width="200"/>
</p>

<h1 align="center">Stasis</h1>

<p align="center">
  <strong>A modern Wayland idle manager that knows when to step back.</strong>
</p>

<p align="center">
  Keep your session in perfect balance—automatically preventing idle when it matters, allowing it when it doesn't.
</p>

<p align="center">
  <img src="https://img.shields.io/github/last-commit/saltnpepper97/stasis?style=for-the-badge&color=%2328A745" alt="GitHub last commit"/>
  <img src="https://img.shields.io/aur/version/stasis?style=for-the-badge" alt="AUR version">
  <img src="https://img.shields.io/badge/License-GPLv3-E5534B?style=for-the-badge" alt="MIT License"/>
  <img src="https://img.shields.io/badge/Wayland-00BFFF?style=for-the-badge&logo=wayland&logoColor=white" alt="Wayland"/>
  <img src="https://img.shields.io/badge/Rust-1.89+-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Rust"/>
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#cli-usage">CLI Usage</a> •
  <a href="#compositor-support">Compositor Support</a> •
  <a href="#contributing">Contributing</a>
</p>

---

## Features

Stasis is not a simple timer-based screen locker.  
It is a **context-aware, event-driven idle manager** built around explicit state and decisions.

- 🧠 Smart idle detection with sequential, configurable timeouts
- 🎵 Media-aware idle handling
  - Optional audio-based detection
  - Differentiates active, paused, and muted streams
- 🌐 Browser activity/media bridge (recommended)
  - Event-driven browser pulses (`browser-activity` / `browser-inactive`)
  - Reliable behavior across Firefox and Chromium with one shared extension codebase
- 🚫 Application-specific inhibitors
  - Prevent idle when selected apps are running
  - Regex-based matching supported
- ⏸️ Wayland idle inhibitor support
  - Honors compositor and application inhibitors
- 🛌 Laptop-aware power handling
  - Optional D-Bus integration for lid events and suspend/resume
- ⚙️ Flexible action plans
  - Startup steps, sequential steps, instant actions, resume hooks
- 🔁 Manual idle inhibition
  - Toggle idle on/off via CLI or status bars (Waybar-friendly)
- 📝 Clean configuration
  - Uses the expressive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language
- ⚡ Live reload
  - Reload configuration without restarting the daemon
- 📜 Structured logging
  - Powered by [eventline](https://github.com/saltnpepper97/eventline) for journaling and traceable logs

---

## Architecture

Stasis is built around a deterministic, event-driven state machine.

There are no hidden timers, background polling loops, or implicit behavior.

    External signals
      ↓
    Event (pure data)
      ↓
    Manager (decision logic)
      ↓
    State (authoritative)
      ↓
    Actions (declarative)
      ↓
    Services (side effects)

Design principles:

- State is authoritative
- Events are pure data
- Managers decide, services act
- Side effects are isolated
- Data flows strictly forward

---

## Installation

### Arch Linux (AUR)

    yay -S stasis
    yay -S stasis-git

### Nix / NixOS (Flakes)

    nix build 'github:saltnpepper97/stasis#stasis'

#### NixOS Notes

**swaylock PAM configuration**

If you use swaylock as your screen locker on NixOS, you must add the following to your NixOS configuration or swaylock will lock the screen but never accept your password to unlock it:

```nix
security.pam.services.swaylock = {};
```

---

### From Source

Dependencies:
- rust / cargo
- wayland (for native input detection)
- dbus (optional, for lid events and suspend/resume handling)
- libnotify (optional, for desktop notifications)
- pulseaudio or pipewire-pulse (optional, for audio/media detection)

Build & install:

    git clone https://github.com/saltnpepper97/stasis
    cd stasis
    cargo build --release --locked
    sudo install -Dm755 target/release/stasis /usr/local/bin/stasis

---

## Quick Start

> [!WARNING]
> **Screen lockers must not be configured to daemonize.**
> Stasis tracks lock state by waiting for the screen locker process to exit. If your locker is set to daemonize (e.g. `swaylock -f` / `daemonize = true`, or a similar option in other lockers), it will detach from stasis immediately and stasis will interpret this as the screen already being unlocked — causing it to loop back to the first step of your plan.
>
> You can confirm this is the issue with `stasis dump` — if the `resume` step fires almost immediately after `lock`, your locker is daemonizing.
>
> **Option 1 — Don't daemonize (simplest):** Remove `daemonize = true` or the `-f` flag from your screen locker config.
>
> **Option 2 — Use `enable_loginctl` mode:** Enable stasis's `loginctl` mode so it tracks lock state via logind signals instead of process lifetime, then use a wrapper script:
> ```bash
> #!/usr/bin/env bash
> loginctl lock-session
> swaylock -f
> ```

Start the daemon:

    stasis

Full quick start guide, configuration examples, and documentation:  
https://saltnpepper97.github.io/stasis-site/

---

## Browser Integration (Recommended)

For reliable browser behavior, Stasis now supports a native browser bridge:

- Browser extension emits `browser-activity` and `browser-inactive` pulses.
- Stasis keeps browser signals in a waiting-for-idle state without inflating inhibitor counts.
- Non-browser media and `inhibit_apps` still use core service counters.

Repository paths:

- Extension scaffold: `browser/extension/`
- Native host scaffold: `browser/native-host/`

Local build/install:

```bash
browser/extension/scripts/build.sh
browser/native-host/scripts/install.sh
```

For Chromium-family browsers, install native host with extension ID:

```bash
browser/native-host/scripts/install.sh --chromium-origin <EXTENSION_ID>
```

---

## Packager Notes (Arch/AUR)

If packaging browser bridge support, include:

- `browser/native-host/stasis_native_host.py`
- `browser/native-host/manifests/*.template`
- `browser/native-host/scripts/install.sh`

Recommended post-install guidance:

- User loads extension (Firefox/Chromium artifact from `browser/extension/dist`).
- User runs native host installer script once (with Chromium extension ID when needed).
- User restarts browser after manifest install.

---

## CLI Usage

    stasis info [--json]
    stasis pause [for <duration> | until <time>]
    stasis resume
    stasis toggle-inhibit
    stasis trigger <step|all>
    stasis list actions
    stasis list profiles
    stasis profile <name|none>
    stasis reload
    stasis stop

---

## Compositor Support

Stasis integrates with each compositor's available IPC and standard Wayland protocols.

| Compositor | Support Status | Notes |
|-----------|----------------|-------|
| **Niri** | ✅ Full Support | Tested and working perfectly |
| **Hyprland** | ✅ Full Support | Native IPC integration |
| **labwc** | ⚠️ Limited | Process-based fallback |
| **River** | ⚠️ Limited | Process-based fallback |
| **Your Favorite** | 🤝 PRs Welcome | Help us expand support |

### River & labwc Notes

These compositors have IPC limitations that affect window enumeration.

- Stasis falls back to process-based detection
- Regex patterns may need adjustment
- Enable verbose logging to inspect detected applications

---

## Contributing

Thank you for helping improve Stasis!

Guidelines:
1. Bug reports and feature requests must start as issues
2. Packaging and compositor support PRs are welcome directly
3. Other changes should be discussed before submission

---

## ❤️ Support Development

If you find this project useful, consider sponsoring its development.

GitHub Sponsors helps ensure continued maintenance, faster bug fixes, and long-term improvements.

➡ https://github.com/sponsors/saltnpepper97

---

## License

Released under the GPLv3 License.

---

<p align="center">
  <sub>Built with ❤️ for the Wayland community</sub><br>
  <sub><i>Keeping your session in perfect balance between active and idle</i></sub>
</p>
