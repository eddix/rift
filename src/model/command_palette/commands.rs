//! Executable commands and shortcuts from the same runtime configuration as hotkeys.

use rift_protocol::{
    Direction, DisplaySelector, LayoutCommand as L, LayoutMode, MetricsCommand,
    ReactorCommand as R, ResizeOrientation, RestoreScope, RestoreSource, WorkspaceSelector,
};
use serde_json::Value;

use super::{PaletteAction, PaletteEntry, PaletteEntryId, PaletteEntryKind};
use crate::actor::wm_controller::{ConfiguredLayoutCommand, WmCmd, WmCommand};
use crate::common::config::Config;
use crate::model::reactor::Command;

fn layout(command: L) -> WmCommand { WmCommand::ReactorCommand(Command::Layout(command)) }
fn reactor(command: R) -> WmCommand { WmCommand::ReactorCommand(Command::Reactor(command)) }

pub fn entries(
    config: &Config,
    workspace_names: &[String],
    displays: &[(String, String)],
) -> Vec<PaletteEntry> {
    let mut commands = Vec::new();
    for index in 0..workspace_names.len() {
        commands.push(layout(L::SwitchToWorkspace(index)));
        for follow in [true, false] {
            commands.push(layout(L::MoveWindowToWorkspace {
                workspace: WorkspaceSelector::Index(index),
                follow,
                window_id: None,
            }));
        }
    }
    commands.extend(
        [
            L::NextWindow,
            L::PrevWindow,
            L::Ascend,
            L::Descend,
            L::ToggleFocusFloating,
            L::ToggleWindowFloating,
            L::ToggleFullscreen,
            L::ToggleFullscreenWithinGaps,
            L::ToggleStack,
            L::ToggleOrientation,
            L::Balance,
            L::UnjoinWindows,
            L::SnapStrip,
            L::SwitchPresetColumnWidth { backwards: false },
            L::SwitchPresetColumnWidth { backwards: true },
            L::CenterSelection,
            L::NextWorkspace(None),
            L::PrevWorkspace(None),
            L::CreateWorkspace,
            L::SwitchToLastWorkspace,
            L::AdjustMasterRatio(0.05),
            L::AdjustMasterRatio(-0.05),
            L::AdjustMasterCount { delta: 1 },
            L::AdjustMasterCount { delta: -1 },
            L::PromoteToMaster,
            L::SwapMasterStack,
            L::ScrollStrip { delta: 100.0 },
            L::ScrollStrip { delta: -100.0 },
            L::ResizeWindowBy { amount: 10.0 },
            L::ResizeWindowBy { amount: -10.0 },
        ]
        .map(layout),
    );
    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        commands.extend(
            [
                L::MoveFocus(direction),
                L::MoveNode(direction),
                L::JoinWindow(direction),
                L::ConsumeOrExpelWindow(direction),
            ]
            .map(layout),
        );
        let selector = DisplaySelector::Direction(direction);
        commands.extend(
            [
                R::FocusDisplay(selector.clone()),
                R::MoveMouseToDisplay(selector.clone()),
                R::MoveWindowToDisplay {
                    selector: selector.clone(),
                    window_id: None,
                },
                R::MoveWorkspaceToDisplay { selector, wrap_around: false },
            ]
            .map(reactor),
        );
    }
    for orientation in [
        ResizeOrientation::Smart,
        ResizeOrientation::Horizontal,
        ResizeOrientation::Vertical,
    ] {
        commands.extend(
            [
                L::ResizeWindowGrow(orientation),
                L::ResizeWindowShrink(orientation),
            ]
            .map(layout),
        );
    }
    for mode in [
        LayoutMode::Traditional,
        LayoutMode::Bsp,
        LayoutMode::Stack,
        LayoutMode::MasterStack,
        LayoutMode::Scrolling,
        LayoutMode::Floating,
    ] {
        commands.push(layout(L::SetWorkspaceLayout { workspace: None, mode }));
    }
    for (uuid, _) in displays {
        let selector = DisplaySelector::Uuid(uuid.clone());
        commands.extend(
            [
                R::FocusDisplay(selector.clone()),
                R::MoveMouseToDisplay(selector.clone()),
                R::MoveWindowToDisplay {
                    selector: selector.clone(),
                    window_id: None,
                },
                R::MoveWorkspaceToDisplay { selector, wrap_around: false },
            ]
            .map(reactor),
        );
    }
    commands.extend(
        [
            R::OpenSettings,
            R::ToggleSpaceActivated,
            R::ToggleFocusUntiled,
            R::CycleUntiledWindows,
            R::ToggleFocusUnmanaged,
            R::CycleUnmanagedWindows,
            R::ResetAllWorkspaces,
            R::ShowMissionControlAll,
            R::DismissMissionControl,
            R::SwitchSpace(Direction::Left),
            R::SwitchSpace(Direction::Right),
            R::CloseWindow { window_server_id: None },
            R::Debug,
            R::Serialize,
            R::SaveAndExit,
        ]
        .map(reactor),
    );
    commands.push(WmCommand::Wm(WmCmd::ReloadConfig));
    commands.push(reactor(R::SaveLayout {
        path: crate::common::config::restore_file(),
    }));
    for scope in [RestoreScope::Workspace, RestoreScope::Space] {
        commands.push(reactor(R::RestoreLayout {
            path: crate::common::config::restore_file(),
            scope,
            source: RestoreSource::CurrentSpace,
        }));
    }
    commands.push(WmCommand::ReactorCommand(Command::Metrics(
        MetricsCommand::ShowTiming,
    )));
    for (mode, _) in &config.binding_mode_specs {
        commands.push(WmCommand::Wm(WmCmd::BindingMode(mode.clone())));
    }
    // Include exact configured arguments, including exec, layout paths, and future commands.
    // Keep dispatch payloads intact; canonicalization is only for equivalence and hints.
    let bindings: Vec<_> = config
        .binding_mode_specs
        .iter()
        .flat_map(|(mode, bindings)| {
            bindings
                .iter()
                .map(move |(key, command)| (mode, key, command, canonical(command, config)))
        })
        .collect();
    let mut result: Vec<PaletteEntry> = Vec::new();
    for command in commands.iter().chain(bindings.iter().map(|(_, _, command, _)| *command)) {
        let canonical = canonical(command, config);
        let id = PaletteEntryId::Command(format!("{canonical:?}"));
        if result.iter().any(|entry| entry.id == id) {
            continue;
        }
        let shortcuts: Vec<_> = bindings
            .iter()
            .filter(|(_, _, _, binding)| &canonical == binding)
            .map(|(mode, key, _, _)| {
                let keys = shortcut_glyphs(key);
                if mode.as_str() == "default" {
                    keys
                } else {
                    format!("{mode}: {keys}")
                }
            })
            .collect();
        let label = label(&canonical, workspace_names, displays);
        let secondary = if shortcuts.is_empty() {
            "Unbound".into()
        } else {
            shortcuts.join(" / ")
        };
        let keywords = serde_json::to_string(&command)
            .into_iter()
            .chain(std::iter::once("command".into()));
        result.push(
            PaletteEntry::new(
                id,
                PaletteEntryKind::Command,
                label,
                secondary,
                keywords,
                None,
                PaletteAction::Command(command.clone()),
            )
            .hidden_when_empty(),
        );
    }
    result
}

fn canonical(command: &WmCommand, config: &Config) -> WmCommand {
    let workspace = |selector: &WorkspaceSelector| match selector {
        WorkspaceSelector::Name(name) => config
            .virtual_workspaces
            .workspace_names
            .iter()
            .position(|candidate| candidate == name)
            .map_or_else(|| selector.clone(), WorkspaceSelector::Index),
        _ => selector.clone(),
    };
    match command {
        WmCommand::Wm(command) => match command {
            WmCmd::ToggleSpaceActivated => reactor(R::ToggleSpaceActivated),
            WmCmd::NextWorkspace => layout(L::NextWorkspace(None)),
            WmCmd::PrevWorkspace => layout(L::PrevWorkspace(None)),
            WmCmd::SwitchToWorkspace(selector) => match workspace(selector) {
                WorkspaceSelector::Index(index) => layout(L::SwitchToWorkspace(index)),
                _ => WmCommand::Wm(command.clone()),
            },
            WmCmd::MoveWindowToWorkspace(selector) => layout(L::MoveWindowToWorkspace {
                workspace: workspace(selector),
                follow: false,
                window_id: None,
            }),
            WmCmd::CreateWorkspace => layout(L::CreateWorkspace),
            WmCmd::SwitchToLastWorkspace => layout(L::SwitchToLastWorkspace),
            WmCmd::ShowMissionControlAll | WmCmd::ShowMissionControlCurrent => {
                reactor(R::ShowMissionControlAll)
            }
            WmCmd::DismissMissionControl => reactor(R::DismissMissionControl),
            WmCmd::ToggleCommandPalette => reactor(R::ToggleCommandPalette),
            WmCmd::ToggleCommandPaletteCommands => reactor(R::ToggleCommandPaletteCommands),
            WmCmd::CloseWindow => reactor(R::CloseWindow { window_server_id: None }),
            WmCmd::ReloadConfig | WmCmd::Exec(_) | WmCmd::BindingMode(_) => {
                WmCommand::Wm(command.clone())
            }
        },
        WmCommand::ConfiguredLayout(ConfiguredLayoutCommand::ToggleWindowFloating(options)) => {
            if *options == Default::default() {
                layout(L::ToggleWindowFloating)
            } else {
                layout(L::ToggleWindowFloatingWithOptions(*options))
            }
        }
        WmCommand::ConfiguredLayout(ConfiguredLayoutCommand::SwitchPresetColumnWidth) => {
            layout(L::SwitchPresetColumnWidth { backwards: false })
        }
        WmCommand::ReactorCommand(Command::Layout(L::MoveWindowToWorkspace {
            workspace: selector,
            follow,
            window_id,
        })) => layout(L::MoveWindowToWorkspace {
            workspace: workspace(selector),
            follow: *follow,
            window_id: *window_id,
        }),
        WmCommand::ReactorCommand(Command::Layout(L::ToggleWindowFloatingWithOptions(options)))
            if *options == Default::default() =>
        {
            layout(L::ToggleWindowFloating)
        }
        WmCommand::ReactorCommand(Command::Reactor(R::ShowMissionControlCurrent)) => {
            reactor(R::ShowMissionControlAll)
        }
        WmCommand::Sequence(commands) => {
            WmCommand::Sequence(commands.iter().map(|command| canonical(command, config)).collect())
        }
        _ => command.clone(),
    }
}

fn label(command: &WmCommand, workspaces: &[String], displays: &[(String, String)]) -> String {
    let workspace_name =
        |index: usize| workspaces.get(index).cloned().unwrap_or_else(|| (index + 1).to_string());
    let workspace = |selector: &WorkspaceSelector| match selector {
        WorkspaceSelector::Index(index) => workspace_name(*index),
        WorkspaceSelector::Name(name) => name.clone(),
    };
    let display = |selector: &DisplaySelector| match selector {
        DisplaySelector::Direction(direction) => format!("{direction:?}"),
        DisplaySelector::Index(index) => (index + 1).to_string(),
        DisplaySelector::Uuid(uuid) => displays
            .iter()
            .find(|(id, _)| id == uuid)
            .map_or_else(|| uuid.clone(), |(_, name)| name.clone()),
    };
    match command {
        WmCommand::ReactorCommand(Command::Layout(command)) => match command {
            L::MoveFocus(direction) => format!("Move Focus → {direction:?}"),
            L::MoveNode(direction) => format!("Move Window → {direction:?}"),
            L::JoinWindow(direction) => format!("Join Window → {direction:?}"),
            L::ConsumeOrExpelWindow(direction) => {
                format!("Consume or Expel Window → {direction:?}")
            }
            L::ResizeWindowGrow(orientation) => format!("Grow Window → {orientation:?}"),
            L::ResizeWindowShrink(orientation) => format!("Shrink Window → {orientation:?}"),
            L::ResizeWindowBy { amount } => format!("Resize Window → {amount:+}"),
            L::ScrollStrip { delta } => format!("Scroll Strip → {delta:+}"),
            L::SwitchPresetColumnWidth { backwards } => {
                let direction = if *backwards { "Previous" } else { "Next" };
                format!("{direction} Preset Column Width")
            }
            L::AdjustMasterRatio(delta) => format!("Adjust Master Ratio → {delta:+}"),
            L::AdjustMasterCount { delta } => format!("Adjust Master Count → {delta:+}"),
            L::NextWorkspace(skip_empty) | L::PrevWorkspace(skip_empty) => {
                let direction = if matches!(command, L::NextWorkspace(_)) {
                    "Next"
                } else {
                    "Previous"
                };
                let suffix = match skip_empty {
                    None => "",
                    Some(true) => " · Skip Empty",
                    Some(false) => " · Include Empty",
                };
                format!("{direction} Workspace{suffix}")
            }
            L::SwitchToWorkspace(index) => format!("Switch Workspace → {}", workspace_name(*index)),
            L::MoveWindowToWorkspace {
                workspace: target,
                follow,
                window_id,
            } => {
                let verb = if *follow {
                    "Move Window and Follow"
                } else {
                    "Move Window"
                };
                let window = window_id.map_or_else(String::new, |id| format!(" {id}"));
                format!("{verb}{window} → {}", workspace(target))
            }
            L::SetWorkspaceLayout { workspace, mode } => {
                let suffix = workspace.map_or_else(String::new, |index| {
                    format!(" · Workspace {}", workspace_name(index))
                });
                format!("Set Layout → {mode}{suffix}")
            }
            _ => serialized_label(command),
        },
        WmCommand::ReactorCommand(Command::Reactor(command)) => match command {
            R::FocusDisplay(target) => format!("Focus Display → {}", display(target)),
            R::MoveMouseToDisplay(target) => {
                format!("Move Pointer to Display → {}", display(target))
            }
            R::MoveWindowToDisplay { selector, window_id } => {
                let window = window_id.map_or_else(String::new, |id| format!(" {id}"));
                format!("Move Window{window} to Display → {}", display(selector))
            }
            R::MoveWorkspaceToDisplay { selector, wrap_around } => {
                let suffix = if *wrap_around { " · Wrap Around" } else { "" };
                format!("Move Workspace to Display → {}{suffix}", display(selector))
            }
            R::ToggleSpaceActivated => "Toggle Tiling on macOS Space".into(),
            R::ShowMissionControlAll => "Show Rift Overview".into(),
            R::DismissMissionControl => "Dismiss Rift Overview".into(),
            R::SwitchSpace(direction) => format!("Switch macOS Space → {direction:?}"),
            R::CloseWindow { window_server_id: None } => "Close Window".into(),
            R::SaveLayout { path } => format!("Save Layout → {}", path.display()),
            R::RestoreLayout { path, scope, source } => {
                let scope = match scope {
                    RestoreScope::Workspace => "Workspace",
                    RestoreScope::Space => "Display",
                };
                let source = match source {
                    RestoreSource::CurrentSpace => "Current Display",
                    RestoreSource::SavedActiveSpace => "Saved Display",
                };
                format!("Restore {scope} Layout → {} · {source}", path.display())
            }
            _ => serialized_label(command),
        },
        WmCommand::ReactorCommand(Command::Metrics(command)) => serialized_label(command),
        WmCommand::Wm(command) => serialized_label(command),
        WmCommand::Sequence(commands) => commands
            .iter()
            .map(|command| label(command, workspaces, displays))
            .collect::<Vec<_>>()
            .join(" → "),
        WmCommand::ConfiguredLayout(command) => serialized_label(command),
    }
}

fn serialized_label(command: &impl serde::Serialize) -> String {
    fn title(value: &str) -> String {
        value
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    match serde_json::to_value(command) {
        Ok(Value::String(name)) => title(&name),
        Ok(Value::Object(object)) => object
            .into_iter()
            .map(|(name, value)| {
                let name = title(&name);
                match value {
                    Value::Null => name,
                    Value::String(value) => format!("{name} → {value}"),
                    value => format!("{name} → {value}"),
                }
            })
            .collect::<Vec<_>>()
            .join(" · "),
        _ => "Rift Command".into(),
    }
}

fn shortcut_glyphs(key: &str) -> String {
    key.split('+')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "alt" | "option" => "⌥",
            "shift" => "⇧",
            "ctrl" | "control" => "⌃",
            "meta" | "cmd" | "command" => "⌘",
            "fn" => "fn",
            "left" | "arrowleft" => "←",
            "right" | "arrowright" => "→",
            "up" | "arrowup" => "↑",
            "down" | "arrowdown" => "↓",
            "comma" => ",",
            "period" => ".",
            "slash" => "/",
            "backslash" => "\\",
            "equal" => "=",
            "minus" => "−",
            "enter" | "return" => "↩",
            "tab" => "⇥",
            "space" => "␣",
            "escape" => "⎋",
            "backspace" => "⌫",
            _ => part.trim(),
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::wm_controller::ExecCmd;
    use crate::common::collections::HashSet;

    fn commands(config: &Config) -> Vec<PaletteEntry> {
        entries(config, &config.virtual_workspaces.workspace_names, &[])
    }

    fn find<'a>(
        entries: &'a [PaletteEntry],
        command: &WmCommand,
        config: &Config,
    ) -> &'a PaletteEntry {
        entries
            .iter()
            .find(|entry| match &entry.action {
                PaletteAction::Command(candidate) => {
                    canonical(candidate, config) == canonical(command, config)
                }
                _ => false,
            })
            .unwrap_or_else(|| panic!("missing command {command:?}"))
    }

    #[test]
    fn catalog_should_include_every_personal_binding_with_its_shortcut() {
        let config = Config::parse(include_str!("../../../config/eddix.toml")).unwrap();
        let entries = commands(&config);
        for (mode, bindings) in &config.binding_mode_specs {
            for (key, command) in bindings {
                let entry = find(&entries, command, &config);
                assert!(
                    entry.secondary.contains(&shortcut_glyphs(key)),
                    "{mode}: {key} missing from {entry:?}"
                );
            }
        }
        let ids: HashSet<_> = entries.iter().map(|entry| &entry.id).collect();
        assert_eq!(entries.len(), ids.len(), "duplicate command rows");
        eprintln!(
            "Personal profile: {} commands, {} bindings",
            entries.len(),
            config.keys.len()
        );
    }

    #[test]
    fn catalog_should_offer_unbound_layout_focus_and_window_commands() {
        let mut config = Config::default();
        config.binding_mode_specs.clear();
        let entries = commands(&config);
        for command in [
            layout(L::ToggleStack),
            layout(L::ToggleOrientation),
            layout(L::UnjoinWindows),
            layout(L::PromoteToMaster),
            layout(L::SwapMasterStack),
            layout(L::SnapStrip),
            layout(L::SetWorkspaceLayout {
                workspace: None,
                mode: LayoutMode::Floating,
            }),
            reactor(R::ToggleFocusUntiled),
            reactor(R::CycleUntiledWindows),
            reactor(R::ToggleFocusUnmanaged),
            reactor(R::CycleUnmanagedWindows),
            reactor(R::OpenSettings),
            reactor(R::CloseWindow { window_server_id: None }),
        ] {
            assert_eq!(find(&entries, &command, &config).secondary, "Unbound");
        }
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            for command in [
                L::MoveFocus(direction),
                L::MoveNode(direction),
                L::JoinWindow(direction),
                L::ConsumeOrExpelWindow(direction),
            ] {
                find(&entries, &layout(command), &config);
            }
        }
    }

    #[test]
    fn equivalent_bindings_should_share_one_row_and_show_their_keymaps() {
        let config = Config::parse(
            r#"
            [settings]
            [keys]
            "Alt + H" = "next_workspace"
            "Alt + L" = { next_workspace = false }
            [binding_modes.resize]
            "H" = "next_workspace"
        "#,
        )
        .unwrap();
        let entries = commands(&config);
        let default = find(&entries, &layout(L::NextWorkspace(None)), &config);
        assert_eq!(default.secondary, "⌥H / resize: H");
        let explicit = find(&entries, &layout(L::NextWorkspace(Some(false))), &config);
        assert_eq!(explicit.secondary, "⌥L");
        assert_ne!(explicit.id, default.id);
    }

    #[test]
    fn workspace_name_alias_should_resolve_without_merging_follow_behavior() {
        let config = Config::parse(
            r#"
            [settings]
            [virtual_workspaces]
            workspace_names = ["Chat", "Code"]
            [keys]
            "Alt + 2" = { switch_to_workspace = "Code" }
            "Alt + Shift + 2" = { move_window_to_workspace = { workspace = "Code", follow = true } }
        "#,
        )
        .unwrap();
        let entries = commands(&config);
        assert_eq!(
            find(&entries, &layout(L::SwitchToWorkspace(1)), &config).secondary,
            "⌥2"
        );
        for follow in [true, false] {
            let entry = find(
                &entries,
                &layout(L::MoveWindowToWorkspace {
                    workspace: WorkspaceSelector::Index(1),
                    follow,
                    window_id: None,
                }),
                &config,
            );
            assert_eq!(entry.secondary, if follow { "⌥⇧2" } else { "Unbound" });
        }
    }

    #[test]
    fn configured_arguments_should_be_preserved_in_executable_actions() {
        let mut config = Config::default();
        let exec = WmCommand::Wm(WmCmd::Exec(ExecCmd::Array(vec![
            "/usr/bin/open".into(),
            "-a".into(),
            "Ghostty".into(),
        ])));
        let custom = layout(L::ResizeWindowBy { amount: 37.0 });
        let options = WmCommand::ConfiguredLayout(ConfiguredLayoutCommand::ToggleWindowFloating(
            rift_protocol::ToggleWindowFloatingOptions {
                center: true,
                size: Some(rift_protocol::FloatingWindowSize::Dimensions { w: 900.0, h: 700.0 }),
            },
        ));
        config.binding_mode_specs = vec![("default".into(), vec![
            ("Alt + Enter".into(), exec.clone()),
            ("Alt + G".into(), custom.clone()),
            ("Alt + T".into(), options.clone()),
        ])];
        let entries = commands(&config);
        for command in [exec, custom, options] {
            assert_eq!(
                find(&entries, &command, &config).action,
                PaletteAction::Command(command)
            );
        }
        assert_eq!(
            find(&entries, &layout(L::ToggleWindowFloating), &config).secondary,
            "Unbound"
        );
    }

    #[test]
    fn fresh_snapshot_should_replace_shortcuts_after_config_reload() {
        let mut config = Config::default();
        let command = reactor(R::ToggleFocusUntiled);
        config.binding_mode_specs =
            vec![("default".into(), vec![("Alt + U".into(), command.clone())])];
        assert_eq!(find(&commands(&config), &command, &config).secondary, "⌥U");
        config.binding_mode_specs[0].1[0].0 = "Ctrl + U".into();
        assert_eq!(find(&commands(&config), &command, &config).secondary, "⌃U");
    }

    #[test]
    fn display_commands_should_identify_the_connected_display() {
        let config = Config::default();
        let entries = entries(&config, &[], &[("display-a".into(), "Built-in".into())]);
        let entry = find(
            &entries,
            &reactor(R::FocusDisplay(DisplaySelector::Uuid("display-a".into()))),
            &config,
        );
        assert_eq!(entry.primary, "Focus Display → Built-in");
    }

    #[test]
    fn shortcut_should_expand_modifier_aliases_and_navigation_keys() {
        assert_eq!(shortcut_glyphs("option + control + shift + ArrowLeft"), "⌥⌃⇧←");
        assert_eq!(shortcut_glyphs("AltRight + Period"), "AltRight.");
    }

    #[test]
    fn catalog_should_preserve_a_configured_command_sequence_and_its_shortcut() {
        let config = Config::parse(
            r#"
            [settings]
            [keys]
            "Alt + B" = ["balance", { binding_mode = "default" }]
        "#,
        )
        .unwrap();
        let command = config.binding_mode_specs[0].1[0].1.clone();
        let entries = commands(&config);
        let entry = find(&entries, &command, &config);
        assert_eq!(entry.action, PaletteAction::Command(command));
        assert_eq!(entry.secondary, "⌥B");
        assert_eq!(entry.primary, "Balance → Binding Mode → default");
        assert_eq!(find(&entries, &layout(L::Balance), &config).secondary, "Unbound");
    }
}
