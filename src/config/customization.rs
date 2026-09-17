//! Persisted customization: the widget grid, named layout presets, and named themes.
//!
//! This module owns every type that describes *how* the launcher looks, as opposed to
//! what it does. It is deliberately free of egui: nothing here renders, so the same
//! structures can be validated, serialized, and unit-tested without a UI thread.
//!
//! Two collections drive customization. [`LayoutConfig::presets`] holds complete named
//! layouts (a grid plus placements for every page); [`ThemeConfig::themes`] holds named
//! palettes. Each is addressed by name rather than index so that reordering or removing
//! entries never silently re-points the active selection at different content.
//!
//! The first entry of each collection is Ferrite's built-in default and reproduces the
//! launcher's original appearance exactly. Defaults are reconstructed rather than
//! mutated, so a user can always return to the shipped look.

use serde::{Deserialize, Serialize};

/// Highest addressable grid row. Rows are virtual; the canvas scrolls.
pub const MAX_LAYOUT_ROWS: u16 = 1_000;
/// Maximum number of widget placements accepted on one page.
pub const MAX_WIDGET_PLACEMENTS: usize = 256;
/// Maximum custom text length, measured in Unicode scalar values.
pub const MAX_WIDGET_TEXT_LENGTH: usize = 2_048;
/// Maximum action-button label length, measured in Unicode scalar values.
pub const MAX_WIDGET_LABEL_LENGTH: usize = 80;
/// Maximum preset/theme name length, measured in Unicode scalar values.
pub const MAX_NAME_LENGTH: usize = 64;
/// Maximum number of saved layout presets.
pub const MAX_PRESETS: usize = 64;
/// Maximum number of saved themes.
pub const MAX_THEMES: usize = 64;

/// The name of the built-in layout preset and the built-in theme.
pub const DEFAULT_NAME: &str = "Ferrite Default";

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Opt-in widget layout, the set of saved presets, and the editor's session flags.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutConfig {
    /// When `false`, consumers render the original fixed shell.
    pub enabled: bool,
    /// Whether the drag-and-drop editor overlay is active.
    pub edit_mode: bool,
    /// Whether the editor paints grid guides behind widgets.
    pub show_grid: bool,
    /// Whether widgets snap to whole cells while dragging.
    pub snap_to_grid: bool,
    /// Name of the entry in [`Self::presets`] currently being rendered.
    pub active_preset: String,
    /// Every saved layout. Always contains the built-in default at index zero.
    pub presets: Vec<LayoutPreset>,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            edit_mode: false,
            show_grid: true,
            snap_to_grid: true,
            active_preset: DEFAULT_NAME.to_owned(),
            presets: vec![LayoutPreset::default()],
        }
    }
}

impl LayoutConfig {
    /// Returns the active preset, falling back to the first entry.
    ///
    /// A missing or renamed selection must never blank the window, so resolution is
    /// total: an empty preset list is repaired by [`Self::ensure_invariants`] before
    /// this is called, and the fallback covers any list mutated between frames.
    pub fn active(&self) -> &LayoutPreset {
        self.presets
            .iter()
            .find(|preset| preset.name == self.active_preset)
            .or_else(|| self.presets.first())
            .unwrap_or(&DEFAULT_PRESET_FALLBACK)
    }

    /// Returns the active preset for editing, inserting the built-in default if absent.
    pub fn active_mut(&mut self) -> &mut LayoutPreset {
        self.ensure_invariants();
        let index = self
            .presets
            .iter()
            .position(|preset| preset.name == self.active_preset)
            .unwrap_or(0);
        &mut self.presets[index]
    }

    /// Guarantees a non-empty preset list and a selection that resolves to a real entry.
    pub fn ensure_invariants(&mut self) {
        if self.presets.is_empty() {
            self.presets.push(LayoutPreset::default());
        }
        if !self
            .presets
            .iter()
            .any(|preset| preset.name == self.active_preset)
        {
            self.active_preset = self.presets[0].name.clone();
        }
    }

    /// Returns a name based on `desired` that no existing preset uses.
    pub fn unique_preset_name(&self, desired: &str) -> String {
        unique_name(
            desired,
            self.presets.iter().map(|preset| preset.name.as_str()),
        )
    }

    pub(super) fn validate(&self) -> Result<(), super::ConfigError> {
        if self.presets.len() > MAX_PRESETS {
            return Err(super::ConfigError::Validation(format!(
                "layout.presets must contain at most {MAX_PRESETS} presets"
            )));
        }
        for (index, preset) in self.presets.iter().enumerate() {
            preset.validate(&format!("layout.presets[{index}]"))?;
            if self.presets[..index]
                .iter()
                .any(|earlier| earlier.name == preset.name)
            {
                return Err(super::ConfigError::Validation(format!(
                    "layout.presets contains more than one preset named '{}'",
                    preset.name
                )));
            }
        }
        Ok(())
    }
}

/// Used only when a caller holds `&LayoutConfig` with an empty preset list.
static DEFAULT_PRESET_FALLBACK: std::sync::LazyLock<LayoutPreset> =
    std::sync::LazyLock::new(LayoutPreset::default);

/// One complete, named, switchable layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayoutPreset {
    pub name: String,
    pub grid: GridConfig,
    pub play: PageLayout,
    pub instances: PageLayout,
    pub mods: PageLayout,
    pub settings: PageLayout,
}

impl Default for LayoutPreset {
    /// Reproduces the launcher's original shell: top bar, page body, status bar.
    ///
    /// These three composite widgets call the same drawing code as the fixed shell, so
    /// the default preset is visually identical to having customization switched off.
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_owned(),
            grid: GridConfig::default(),
            play: PageLayout::shell(),
            instances: PageLayout::shell(),
            mods: PageLayout::shell(),
            settings: PageLayout::shell(),
        }
    }
}

impl LayoutPreset {
    /// A preset whose Play page is split into individually movable pieces.
    ///
    /// Offered alongside the default so a user can start customizing without first
    /// having to disassemble the monolithic page body themselves.
    pub fn detailed() -> Self {
        Self {
            name: "Ferrite Detailed".to_owned(),
            grid: GridConfig::default(),
            play: PageLayout {
                placements: vec![
                    place(Widget::Logo, 1, 1, 3, 1),
                    place(Widget::NavBar, 4, 1, 5, 1),
                    place(Widget::AccountButton, 9, 1, 2, 1),
                    place(
                        Widget::NavButton {
                            page: LayoutPage::Settings,
                        },
                        11,
                        1,
                        2,
                        1,
                    ),
                    place(Widget::HeroBanner, 1, 2, 12, 4),
                    place(Widget::LaunchButton, 1, 6, 12, 1),
                    place(Widget::SelectedInstance, 1, 7, 6, 3),
                    place(Widget::LauncherStatus, 7, 7, 6, 3),
                    place(Widget::StatusBar, 1, 10, 12, 1),
                ],
            },
            instances: PageLayout::shell(),
            mods: PageLayout::shell(),
            settings: PageLayout::shell(),
        }
    }

    pub(super) fn validate(&self, path: &str) -> Result<(), super::ConfigError> {
        validate_name(&self.name, &format!("{path}.name"))?;
        self.grid.validate(path)?;
        for (name, page) in [
            ("play", &self.play),
            ("instances", &self.instances),
            ("mods", &self.mods),
            ("settings", &self.settings),
        ] {
            page.validate(&format!("{path}.{name}"), &self.grid)?;
        }
        Ok(())
    }
}

/// Shared dimensions for one preset's page grids.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GridConfig {
    /// Number of columns in the inclusive range `1..=24`.
    pub columns: u8,
    /// Height of one row in logical pixels, in the range `24..=512`.
    pub row_height: u16,
    /// Gap between cells in logical pixels, in the range `0..=128`.
    pub gap: u16,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            columns: 12,
            row_height: 64,
            gap: 12,
        }
    }
}

impl GridConfig {
    pub(super) fn validate(&self, path: &str) -> Result<(), super::ConfigError> {
        if !(1..=24).contains(&self.columns) {
            return Err(super::ConfigError::Validation(format!(
                "{path}.grid.columns must be between 1 and 24"
            )));
        }
        if !(24..=512).contains(&self.row_height) {
            return Err(super::ConfigError::Validation(format!(
                "{path}.grid.row_height must be between 24 and 512"
            )));
        }
        if self.gap > 128 {
            return Err(super::ConfigError::Validation(format!(
                "{path}.grid.gap must be between 0 and 128"
            )));
        }
        Ok(())
    }
}

/// Ordered widget placements for one page.
///
/// Placements may overlap intentionally; later entries paint above earlier entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PageLayout {
    pub placements: Vec<WidgetPlacement>,
}

impl PageLayout {
    /// The original three-band shell used by the built-in default preset.
    pub fn shell() -> Self {
        Self {
            placements: vec![
                place(Widget::TopBar, 1, 1, 12, 1),
                place(Widget::PageBody, 1, 2, 12, 10),
                place(Widget::StatusBar, 1, 12, 12, 1),
            ],
        }
    }

    /// Returns the last occupied row, used to size the scrollable canvas.
    pub fn last_row(&self) -> u16 {
        self.placements
            .iter()
            .filter(|placement| placement.enabled)
            .map(WidgetPlacement::last_row)
            .max()
            .unwrap_or(1)
            .min(MAX_LAYOUT_ROWS)
    }

    pub(super) fn validate(&self, path: &str, grid: &GridConfig) -> Result<(), super::ConfigError> {
        if self.placements.len() > MAX_WIDGET_PLACEMENTS {
            return Err(super::ConfigError::Validation(format!(
                "{path}.placements must contain at most {MAX_WIDGET_PLACEMENTS} widgets"
            )));
        }
        for (index, placement) in self.placements.iter().enumerate() {
            placement.validate(&format!("{path}.placements[{index}]"), grid)?;
        }
        Ok(())
    }
}

/// One widget's 1-based position, span, and per-instance styling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WidgetPlacement {
    pub widget: Widget,
    pub column: u8,
    pub row: u16,
    pub width: u8,
    pub height: u16,
    pub enabled: bool,
    pub style: WidgetStyle,
}

impl Default for WidgetPlacement {
    fn default() -> Self {
        Self {
            widget: Widget::default(),
            column: 1,
            row: 1,
            width: 1,
            height: 1,
            enabled: true,
            style: WidgetStyle::default(),
        }
    }
}

impl WidgetPlacement {
    /// Returns the last row this placement occupies, saturating at the grid maximum.
    pub fn last_row(&self) -> u16 {
        self.row.saturating_add(self.height).saturating_sub(1)
    }

    /// Returns the last column this placement occupies.
    pub fn last_column(&self) -> u8 {
        self.column.saturating_add(self.width).saturating_sub(1)
    }

    /// Whether two placements cover any shared cell, using half-open coordinates.
    pub fn overlaps(&self, other: &Self) -> bool {
        u16::from(self.column) < u16::from(other.column) + u16::from(other.width)
            && u16::from(other.column) < u16::from(self.column) + u16::from(self.width)
            && u32::from(self.row) < u32::from(other.row) + u32::from(other.height)
            && u32::from(other.row) < u32::from(self.row) + u32::from(self.height)
    }

    /// Clamps the placement into a grid, preserving span where the column allows.
    pub fn clamp_to(&mut self, grid: &GridConfig) {
        let columns = grid.columns.max(1);
        self.column = self.column.clamp(1, columns);
        self.width = self.width.clamp(1, columns - self.column + 1);
        self.row = self.row.clamp(1, MAX_LAYOUT_ROWS);
        self.height = self
            .height
            .clamp(1, MAX_LAYOUT_ROWS - self.row.saturating_sub(1));
    }

    pub(super) fn validate(&self, path: &str, grid: &GridConfig) -> Result<(), super::ConfigError> {
        if self.column == 0 || self.row == 0 || self.width == 0 || self.height == 0 {
            return Err(super::ConfigError::Validation(format!(
                "{path} column, row, width, and height must be greater than zero"
            )));
        }
        if u16::from(self.last_column()) > u16::from(grid.columns) {
            return Err(super::ConfigError::Validation(format!(
                "{path} extends beyond the {}-column grid",
                grid.columns
            )));
        }
        if u32::from(self.row) + u32::from(self.height) - 1 > u32::from(MAX_LAYOUT_ROWS) {
            return Err(super::ConfigError::Validation(format!(
                "{path} extends beyond maximum row {MAX_LAYOUT_ROWS}"
            )));
        }
        self.widget.validate(path)
    }
}

/// Per-placement appearance, applied by the renderer before the widget draws.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WidgetStyle {
    pub frame: WidgetFrame,
    /// Inner padding in logical pixels, capped at 64 by validation.
    pub padding: u8,
}

/// Background treatment drawn behind a placement.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WidgetFrame {
    /// No background; the widget draws whatever it drew in the fixed shell.
    #[default]
    None,
    /// The theme's card color with the configured corner radius.
    Card,
    /// The theme's sidebar color, matching the original top bar.
    Sidebar,
    /// A hairline outline in the accent color with no fill.
    Outline,
}

impl WidgetFrame {
    pub const ALL: &'static [Self] = &[Self::None, Self::Card, Self::Sidebar, Self::Outline];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No frame",
            Self::Card => "Card",
            Self::Sidebar => "Sidebar",
            Self::Outline => "Outline",
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// Content rendered by a layout placement.
///
/// The set is closed: configuration selects a variant, it never names code. Composite
/// variants ([`Widget::TopBar`], [`Widget::PageBody`]) call the same functions the fixed
/// shell does, which is what keeps the default preset pixel-identical to the old UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Widget {
    // Composite — whole bands of the original shell.
    TopBar,
    #[default]
    PageBody,
    StatusBar,

    // Top-bar pieces.
    Logo,
    NavBar,
    NavButton {
        page: LayoutPage,
    },
    AccountButton,

    // Play-page pieces.
    HeroBanner,
    InstanceSelector,
    LaunchButton,
    SelectedInstance,
    AccountSummary,
    LauncherStatus,
    UpdateBanner,

    // Whole-page bodies, for layouts that keep one page monolithic and split another.
    InstancesBody,
    ModsBody,
    SettingsBody,

    // Free-form.
    Text {
        text: String,
    },
    ActionButton {
        label: String,
        action: WidgetAction,
    },
    Spacer,
    Separator,
}

impl Widget {
    pub(super) fn validate(&self, path: &str) -> Result<(), super::ConfigError> {
        match self {
            Self::Text { text } => {
                validate_display_text(text, MAX_WIDGET_TEXT_LENGTH, &format!("{path}.widget.text"))
            }
            Self::ActionButton { label, .. } => validate_display_text(
                label,
                MAX_WIDGET_LABEL_LENGTH,
                &format!("{path}.widget.label"),
            ),
            _ => Ok(()),
        }
    }
}

/// Top-level page addressable by a navigation action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPage {
    #[default]
    Play,
    Instances,
    Mods,
    Settings,
}

impl LayoutPage {
    pub const ALL: &'static [Self] = &[Self::Play, Self::Instances, Self::Mods, Self::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Instances => "Instances",
            Self::Mods => "Mods",
            Self::Settings => "Settings",
        }
    }
}

/// Operation performed by a button widget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WidgetAction {
    #[default]
    Launch,
    Navigate {
        page: LayoutPage,
    },
    StopGame,
    CreateInstance,
    ImportPack,
    ExportPack,
    OpenAccount,
    OpenConfigFolder,
    Settings,
}

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

/// Theme selection plus every saved palette.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: ThemePreset,
    /// Name of the entry in [`Self::themes`] used when `preset` is [`ThemePreset::Saved`].
    pub active_theme: String,
    /// Scratch palette edited by the appearance tab before it is saved under a name.
    pub custom_palette: ThemePalette,
    pub themes: Vec<NamedTheme>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            preset: ThemePreset::Legacy,
            active_theme: DEFAULT_NAME.to_owned(),
            custom_palette: ThemePalette::default(),
            themes: vec![NamedTheme::default(), NamedTheme::light()],
        }
    }
}

impl ThemeConfig {
    /// Returns the saved theme named by [`Self::active_theme`], if it still exists.
    pub fn active(&self) -> Option<&NamedTheme> {
        self.themes
            .iter()
            .find(|theme| theme.name == self.active_theme)
    }

    /// Returns a name based on `desired` that no existing theme uses.
    pub fn unique_theme_name(&self, desired: &str) -> String {
        unique_name(desired, self.themes.iter().map(|theme| theme.name.as_str()))
    }

    pub(super) fn validate(&self) -> Result<(), super::ConfigError> {
        if self.themes.len() > MAX_THEMES {
            return Err(super::ConfigError::Validation(format!(
                "appearance.theme_config.themes must contain at most {MAX_THEMES} themes"
            )));
        }
        self.custom_palette
            .validate("appearance.theme_config.custom_palette")?;
        for (index, theme) in self.themes.iter().enumerate() {
            let path = format!("appearance.theme_config.themes[{index}]");
            validate_name(&theme.name, &format!("{path}.name"))?;
            theme.palette.validate(&format!("{path}.palette"))?;
            if theme.corner_radius > 32 {
                return Err(super::ConfigError::Validation(format!(
                    "{path}.corner_radius must be between 0 and 32"
                )));
            }
            if self.themes[..index]
                .iter()
                .any(|earlier| earlier.name == theme.name)
            {
                return Err(super::ConfigError::Validation(format!(
                    "appearance.theme_config.themes contains more than one theme named '{}'",
                    theme.name
                )));
            }
        }
        Ok(())
    }
}

/// Which palette source resolves the launcher's colors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreset {
    /// Honors the legacy `appearance.theme` and `appearance.accent` fields.
    #[default]
    Legacy,
    /// Ferrite's built-in dark appearance.
    Dark,
    /// Ferrite's built-in light appearance.
    Light,
    /// The unsaved scratch palette in [`ThemeConfig::custom_palette`].
    Custom,
    /// A palette saved under a name in [`ThemeConfig::themes`].
    Saved,
}

impl ThemePreset {
    pub const ALL: &'static [Self] = &[
        Self::Legacy,
        Self::Dark,
        Self::Light,
        Self::Custom,
        Self::Saved,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "Classic / existing settings",
            Self::Dark => "Ferrite Dark",
            Self::Light => "Ferrite Light",
            Self::Custom => "Custom palette",
            Self::Saved => "Saved theme",
        }
    }
}

/// A palette stored under a user-chosen name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NamedTheme {
    pub name: String,
    pub palette: ThemePalette,
    /// Whether UI that branches on brightness should treat this palette as light.
    pub light: bool,
    pub corner_radius: u8,
}

impl Default for NamedTheme {
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_owned(),
            palette: ThemePalette::default(),
            light: false,
            corner_radius: 8,
        }
    }
}

impl NamedTheme {
    /// The built-in light counterpart, matching the original light-mode colors.
    pub fn light() -> Self {
        Self {
            name: "Ferrite Light".to_owned(),
            palette: ThemePalette {
                background: "#F2F4F8".to_owned(),
                sidebar: "#E2E6ED".to_owned(),
                card: "#FFFFFF".to_owned(),
                accent: "#FF6600".to_owned(),
                text: "#000000".to_owned(),
                muted: "#5A5F6A".to_owned(),
            },
            light: true,
            corner_radius: 8,
        }
    }
}

/// Colors available to theme-aware UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemePalette {
    pub background: String,
    pub sidebar: String,
    pub card: String,
    pub accent: String,
    pub text: String,
    pub muted: String,
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            background: "#121418".to_owned(),
            sidebar: "#191C22".to_owned(),
            card: "#1F232A".to_owned(),
            accent: "#FF6600".to_owned(),
            text: "#FFFFFF".to_owned(),
            muted: "#969BA5".to_owned(),
        }
    }
}

impl ThemePalette {
    pub(super) fn validate(&self, path: &str) -> Result<(), super::ConfigError> {
        for (name, color) in [
            ("background", &self.background),
            ("sidebar", &self.sidebar),
            ("card", &self.card),
            ("accent", &self.accent),
            ("text", &self.text),
            ("muted", &self.muted),
        ] {
            if !super::is_rgb_color(color) {
                return Err(super::ConfigError::Validation(format!(
                    "{path}.{name} must be a color in #RRGGBB format"
                )));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Builds a placement with the common defaults, used by the built-in presets.
fn place(widget: Widget, column: u8, row: u16, width: u8, height: u16) -> WidgetPlacement {
    WidgetPlacement {
        widget,
        column,
        row,
        width,
        height,
        ..WidgetPlacement::default()
    }
}

/// Appends a numeric suffix until the name is unused, so saving never overwrites.
fn unique_name<'a>(desired: &str, existing: impl Iterator<Item = &'a str> + Clone) -> String {
    let trimmed = desired.trim();
    let base = if trimmed.is_empty() {
        "Untitled"
    } else {
        trimmed
    };
    if !existing.clone().any(|name| name == base) {
        return base.to_owned();
    }
    (2..)
        .map(|suffix| format!("{base} {suffix}"))
        .find(|candidate| !existing.clone().any(|name| name == candidate))
        .unwrap_or_else(|| base.to_owned())
}

fn validate_name(value: &str, path: &str) -> Result<(), super::ConfigError> {
    validate_display_text(value, MAX_NAME_LENGTH, path)
}

fn validate_display_text(
    value: &str,
    maximum: usize,
    path: &str,
) -> Result<(), super::ConfigError> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > maximum {
        return Err(super::ConfigError::Validation(format!(
            "{path} must contain between 1 and {maximum} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_reproduces_the_original_shell() {
        let preset = LayoutPreset::default();
        let kinds: Vec<_> = preset
            .play
            .placements
            .iter()
            .map(|placement| placement.widget.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![Widget::TopBar, Widget::PageBody, Widget::StatusBar]
        );
        assert!(
            preset
                .play
                .placements
                .iter()
                .all(|placement| placement.column == 1 && placement.width == 12)
        );
    }

    #[test]
    fn active_preset_falls_back_instead_of_blanking_the_window() {
        let mut layout = LayoutConfig::default();
        layout.active_preset = "deleted by hand".to_owned();
        assert_eq!(layout.active().name, DEFAULT_NAME);
        layout.ensure_invariants();
        assert_eq!(layout.active_preset, DEFAULT_NAME);
    }

    #[test]
    fn empty_preset_list_is_repaired_before_editing() {
        let mut layout = LayoutConfig {
            presets: Vec::new(),
            ..LayoutConfig::default()
        };
        assert_eq!(layout.active_mut().name, DEFAULT_NAME);
        assert_eq!(layout.presets.len(), 1);
    }

    #[test]
    fn saving_never_overwrites_an_existing_name() {
        let layout = LayoutConfig::default();
        assert_eq!(layout.unique_preset_name(DEFAULT_NAME), "Ferrite Default 2");
        assert_eq!(layout.unique_preset_name("  "), "Untitled");
    }

    #[test]
    fn clamping_preserves_span_where_the_column_allows() {
        let grid = GridConfig {
            columns: 6,
            ..GridConfig::default()
        };
        let mut placement = place(Widget::Spacer, 5, 1, 12, 1);
        placement.clamp_to(&grid);
        assert_eq!((placement.column, placement.width), (5, 2));
    }

    #[test]
    fn duplicate_preset_names_are_rejected_by_validation() {
        let mut layout = LayoutConfig::default();
        layout.presets.push(LayoutPreset::default());
        assert!(layout.validate().is_err());
    }

    #[test]
    fn overlap_uses_half_open_grid_coordinates() {
        assert!(place(Widget::Spacer, 1, 1, 2, 2).overlaps(&place(Widget::Spacer, 2, 2, 2, 2)));
        assert!(!place(Widget::Spacer, 1, 1, 2, 2).overlaps(&place(Widget::Spacer, 3, 1, 2, 2)));
        assert!(!place(Widget::Spacer, 1, 1, 2, 2).overlaps(&place(Widget::Spacer, 1, 3, 2, 2)));
    }
}
