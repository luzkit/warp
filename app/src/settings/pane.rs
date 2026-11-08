use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(
    description = "How the drag handle is displayed on split terminal panes.",
    rename_all = "snake_case"
)]
pub enum PaneDragHandleVisibility {
    #[default]
    OnHover,
    AlwaysVisible,
    Minimal,
}

define_settings_group!(PaneSettings, settings: [
    should_dim_inactive_panes: ShouldDimInactivePanes {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.panes.should_dim_inactive_panes",
        description: "Whether inactive panes are visually dimmed.",
    },
    focus_panes_on_hover: FocusPaneOnHover {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.panes.focus_pane_on_hover",
        description: "Whether panes are focused when hovered over.",
    },
    pane_drag_handle_visibility: PaneDragHandleVisibilitySetting {
        type: PaneDragHandleVisibility,
        default: PaneDragHandleVisibility::default(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.panes.drag_handle_visibility",
        description: "How the drag handle is displayed on split terminal panes.",
    },
    focus_moved_docking_panes: FocusMovedDockingPanes {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.panes.focus_moved_docking_panes",
        description: "Whether moved panes receive focus after docking.",
    },
    announce_pane_docking: AnnouncePaneDocking {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.panes.announce_pane_docking",
        description: "Whether assistive technology announces pane docking actions.",
    },
]);
