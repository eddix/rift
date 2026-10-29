<div align="center">

# rift
  <p>rift is a tiling window manager for macOS that focuses on performance and usability. </p>
  <img src="assets/demo.gif" alt="Rift demo" />

  <p>
    <a href="https://github.com/acsandmann/rift/actions/workflows/rust.yml">
      <img src="https://img.shields.io/github/actions/workflow/status/acsandmann/rift/rust.yml?style=flat-square" alt="Rust CI Status" />
    </a>
    <a href="https://github.com/acsandmann/rift/commits/main">
      <img src="https://img.shields.io/github/last-commit/acsandmann/rift?style=flat-square" alt="Last Commit" />
    </a>
    <a href="https://github.com/acsandmann/rift/issues">
      <img src="https://img.shields.io/github/issues/acsandmann/rift?style=flat-square" alt="Open Issues" />
    </a>
    <a href="https://github.com/acsandmann/rift/stargazers">
      <img src="https://img.shields.io/github/stars/acsandmann/rift?style=flat-square" alt="GitHub stars" />
    </a>
    <a href="https://matrix.to/#/%23rift:matrix.org">
      <img src="https://img.shields.io/matrix/rift%3Amatrix.org?style=flat-square" alt="Matrix" />
    </a>
  </p>
</div>

## features
- multiple layout styles
  - tiling (i3/sway-like)
  - binary space partitioning (bspwm-like)
  - floating (independent window frames with optional stacks)
  - master-stack (dwm-like)
  - scrolling columns (niri-style)
  - stack (accordion)
- menubar icon that opens a menu for switching workspaces, changing layouts, and accessing quick rift controls <details> <summary><sup>click to see the menu bar icon</sup></summary><img src="assets/menu_menu.png" alt="Rift menu bar icon" /></details>
- save and restore layouts from the menu bar or CLI, with reusable layouts listed from a configurable folder <details> <summary><sup>click to see the menu</sup></summary><img src="assets/menu_layouts.png" alt="Rift menu for restoring layouts" /></details>
- niri-style mission control for viewing workspaces and dragging windows between them
 <!--<details><summary><sup>click to see mission control</sup></summary><img src="assets/mission_control.png" alt="Rift Mission Control view" /></details> -->
- focus follows the mouse with auto raise
<!-- - drag windows over one another to swap positions -->
- does **not** require disabling SIP
- native macOS tab support
- performant animations <sup>(as seen in the [demo](#rift))</sup>
- switch to next/previous workspace with trackpad gestures <sup>(just like native macOS)</sup>
- hot reloadable configuration
- mach port based IPC for communicating with rift from <a href="https://acsandmann.github.io/rift-docs/ecosystem/plugins/">third-party programs</a> (sketchybar, etc)
- works with "Displays have separate Spaces" enabled (unlike all other major WMs)

## quick start
Get up and running via the docs:
<br>

[<kbd><br>config<br></kbd>][config_link]

[<kbd><br>quick start<br></kbd>][quick_start]
<br>

## eddix build and configuration

The `eddix/main` branch contains the downstream features used by the personal
configuration in [`config/eddix.toml`](config/eddix.toml), including workspace
display affinity, borders, the command palette, and AX identifier rules.

To use this build on another Mac:

```sh
git clone --branch eddix/main https://github.com/eddix/rift.git
cd rift
cargo build --locked --release --bins
mkdir -p "$HOME/.local/bin" "$HOME/.config/rift"
```

If that Mac already runs a locally signed Rift, sign the new binaries with its
existing code-signing identity before replacing them. Back up any existing
configuration, then install:

```sh
install -m 755 target/release/rift target/release/rift-cli "$HOME/.local/bin/"
cp config/eddix.toml "$HOME/.config/rift/config.toml"
"$HOME/.local/bin/rift" service start
```

Use `service restart` when updating an existing service. A fresh installation
needs Accessibility permission. Display rules use `built_in` and
`external_primary`, without machine-specific display UUIDs; workspace 5 remains
an independent scratch workspace on each display.

The focused-window border uses a separate color for each window type:

```toml
[settings.ui.border]
color = 0xff7D9C65
tiled_color = 0xff7D9C65
floating_color = 0xff81A1C1
unmanaged_color = 0xffEBCB8B
```

Colors use ARGB values. Each omitted type uses `color` as its fallback. Changes
apply through config hot reload. Toggling a window between tiled and floating
states also updates its border color.

`Alt+U` switches focus between tiled windows and untiled windows (floating or
unmanaged). It returns to the most recently focused tiled window. `Alt+Shift+U`
cycles through untiled windows. Candidates are visible, non-minimized standard
top-level windows on the active display. Managed windows must belong to its
active workspace. The commands do not change window management or workspace
assignments.

The corresponding CLI commands are `rift-cli execute window toggle-focus-untiled`
and `rift-cli execute window cycle-untiled`. The legacy unmanaged-only commands
remain available.

`Alt+Shift+P` opens the command palette. It includes window navigation, grouping,
resizing, all six layout modes, master/stack controls, workspace and display
commands, and every command configured in any keymap. The right column shows
shortcuts from the current configuration. `Unbound` means no shortcut is assigned;
shortcuts from other keymaps include the keymap name. Commands with different
arguments, such as moving a window with or without following it, remain separate.
Shortcut changes apply through config hot reload.

`Alt+O` toggles Rift Overview. Window previews require Screen Recording
permission; after granting it in macOS, open Overview again. Enabling Overview
requires restarting Rift.

## community

Join [#rift:matrix.org](https://matrix.to/#/#rift:matrix.org) for discussion, support, and development.

## support

If rift is part of your daily workflow, consider [sponsoring its development](https://github.com/sponsors/acsandmann).

## motivation
Aerospace worked well for me, but I missed animations and the ability to use fullscreen on one display while working on the other. I also prefer leveraging private/undocumented APIs as they tend to be more reliable (due to the OS being built on them and all the public APIs) and performant.
<sup><sup>for more on why rift exists and what rift strives to do, see the [manifesto](manifesto.md)</sup></sup>


## credits
Rift began as a fork (and is licensed as such) of <a href="https://github.com/glide-wm/glide">glide-wm</a> but has since diverged significantly. It uses private APIs reverse engineered by yabai and other projects. It is not affiliated with glide-wm or yabai.


<!---------------------------------------------------------------------------->

[config_link]: https://acsandmann.github.io/rift-docs/reference/configuration/
[quick_start]: https://acsandmann.github.io/rift-docs/quick-start/
