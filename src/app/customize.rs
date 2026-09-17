//! The grid renderer, the drag-and-drop editor, and the preset manager.
//!
//! Rendering and editing share one geometry function, [`cell_rect`], so a widget is
//! always dragged to exactly the cell it will be drawn in.
//!
//! The editor never mutates configuration from inside an egui closure. Each frame it
//! clones the active page's placements, collects at most one [`Edit`] from the pointer,
//! applies it to the clone, and writes the result back once every closure has finished.
//! Writes to disk are deliberately rarer than writes to memory: a drag updates the live
//! layout on every frame but only reaches `config.toml` when the pointer is released,
//! which keeps a two-second drag from producing a hundred file writes.

use super::widgets::{
    ACTIONS, WIDGET_REGISTRY, WidgetGroup, WidgetKind, action_label, layout_page_from_page,
    page_from_layout, widget_kind, widget_label,
};
use super::{Ferrite, MUTED};
use crate::config::{
    GridConfig, LayoutPage, LayoutPreset, MAX_LAYOUT_ROWS, MAX_WIDGET_LABEL_LENGTH,
    MAX_WIDGET_PLACEMENTS, MAX_WIDGET_TEXT_LENGTH, PageLayout, Widget, WidgetFrame,
    WidgetPlacement,
};
use eframe::egui::{self, Color32, RichText};

/// Width of the resize handle drawn in the bottom-right corner of a selected widget.
const HANDLE: f32 = 14.0;

/// What the pointer is currently doing to a placement.
#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    Resize,
}

/// In-flight pointer interaction, held in egui's temporary memory.
///
/// The original placement is captured at drag start so every frame computes an absolute
/// position from the total pointer delta. Accumulating per-frame deltas instead would
/// let rounding drift the widget away from the cursor over a long drag.
#[derive(Clone, Copy)]
struct Drag {
    index: usize,
    mode: DragMode,
    origin: egui::Pos2,
    start_column: u8,
    start_row: u16,
    start_width: u8,
    start_height: u16,
}

/// One discrete change to the active page, applied after egui releases its borrows.
enum Edit {
    Select(Option<usize>),
    Add(WidgetKind),
    Remove(usize),
    Duplicate(usize),
    Raise(usize),
    Lower(usize),
    Replace(usize, WidgetPlacement),
}

impl Ferrite {
    /// Whether the configurable shell replaces the fixed shell this frame.
    pub(super) fn custom_layout_enabled(&self) -> bool {
        self.config.layout.enabled
    }

    /// Draws the active preset's widgets for the current page.
    pub(super) fn render_layout(&mut self, ui: &mut egui::Ui) {
        let editing = self.config.layout.enabled && self.config.layout.edit_mode;
        let page = layout_page_from_page(self.current_page);
        let grid = self.config.layout.active().grid;
        let placements = page_layout(self.config.layout.active(), page)
            .placements
            .clone();

        let rows = page_layout(self.config.layout.active(), page)
            .last_row()
            // Leave a spare band in edit mode so there is always somewhere to drop.
            .saturating_add(if editing { 2 } else { 0 })
            .min(MAX_LAYOUT_ROWS);
        let height = f32::from(rows) * f32::from(grid.row_height)
            + f32::from(rows.saturating_sub(1)) * f32::from(grid.gap);
        let (canvas, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );

        if editing && self.config.layout.show_grid {
            self.paint_grid_guides(ui, canvas, &grid, rows);
        }

        let selected = selected_index(ui);
        let mut action = None;
        let mut edit = None;

        for (index, placement) in placements.iter().enumerate() {
            if !placement.enabled && !editing {
                continue;
            }
            let Some(rect) = cell_rect(canvas, &grid, placement) else {
                continue;
            };

            ui.scope_builder(
                egui::UiBuilder::new()
                    .id_salt(("layout_widget", index))
                    .max_rect(rect),
                |ui| {
                    ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                    ui.set_min_size(rect.size());
                    if editing {
                        // Widgets stay visible but stop responding, so clicking a Launch
                        // button while arranging it cannot start the game.
                        ui.disable();
                        if !placement.enabled {
                            ui.multiply_opacity(0.35);
                        }
                    }
                    if let Some(requested) = self.render_widget(ui, &placement.widget, &placement.style)
                    {
                        action.get_or_insert(requested);
                    }
                },
            );

            if editing && edit.is_none() {
                edit = self.widget_overlay(ui, canvas, &grid, index, placement, selected == Some(index));
            }
        }

        if editing {
            if let Some(requested) = self.editor_toolbar(ui, page, selected, &placements) {
                edit.get_or_insert(requested);
            }
            if let Some(index) = selected {
                if let Some(placement) = placements.get(index) {
                    if let Some(requested) = self.widget_properties(ui, index, placement) {
                        edit.get_or_insert(requested);
                    }
                }
            }
        }

        if let Some(edit) = edit {
            self.apply_edit(ui, page, edit);
        }
        if let Some(action) = action {
            self.perform_widget_action(action);
        }
    }

    /// Paints cell guides behind the widgets so empty space is visible while editing.
    fn paint_grid_guides(
        &self,
        ui: &egui::Ui,
        canvas: egui::Rect,
        grid: &GridConfig,
        rows: u16,
    ) {
        let painter = ui.painter();
        let stroke = egui::Stroke::new(1.0, self.muted_color().gamma_multiply(0.25));
        for row in 1..=rows {
            for column in 1..=grid.columns {
                let cell = WidgetPlacement {
                    column,
                    row,
                    width: 1,
                    height: 1,
                    ..WidgetPlacement::default()
                };
                if let Some(rect) = cell_rect(canvas, grid, &cell) {
                    painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
                }
            }
        }
    }

    /// Handles selection, dragging, and resizing for one placement.
    fn widget_overlay(
        &mut self,
        ui: &mut egui::Ui,
        canvas: egui::Rect,
        grid: &GridConfig,
        index: usize,
        placement: &WidgetPlacement,
        is_selected: bool,
    ) -> Option<Edit> {
        let Some(rect) = cell_rect(canvas, grid, placement) else {
            return None;
        };
        let accent = self.accent_color();
        let handle_rect = egui::Rect::from_min_size(
            rect.right_bottom() - egui::vec2(HANDLE, HANDLE),
            egui::vec2(HANDLE, HANDLE),
        );

        let body = ui.interact(
            rect,
            ui.id().with(("layout_move", index)),
            egui::Sense::click_and_drag(),
        );
        let handle = ui.interact(
            handle_rect,
            ui.id().with(("layout_resize", index)),
            egui::Sense::drag(),
        );

        // Outline every widget faintly and the selected one strongly, so the editor
        // shows structure that is invisible in normal use.
        let painter = ui.painter();
        painter.rect_stroke(
            rect,
            f32::from(self.corner_radius()),
            egui::Stroke::new(
                if is_selected { 2.0 } else { 1.0 },
                if is_selected {
                    accent
                } else {
                    self.muted_color().gamma_multiply(0.6)
                },
            ),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.left_top() + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            widget_label(&placement.widget),
            egui::FontId::proportional(11.0),
            if is_selected { accent } else { self.muted_color() },
        );
        if is_selected {
            painter.rect_filled(handle_rect, 2.0, accent);
        }
        if body.hovered() || handle.hovered() {
            ui.ctx().set_cursor_icon(if handle.hovered() {
                egui::CursorIcon::ResizeNwSe
            } else {
                egui::CursorIcon::Grab
            });
        }

        if body.clicked() {
            return Some(Edit::Select(Some(index)));
        }

        let drag_id = ui.id().with("layout_drag_state");
        let mode = if handle.drag_started() {
            Some(DragMode::Resize)
        } else if body.drag_started() {
            Some(DragMode::Move)
        } else {
            None
        };
        if let Some(mode) = mode {
            if let Some(origin) = ui.ctx().pointer_interact_pos() {
                ui.data_mut(|data| {
                    data.insert_temp(
                        drag_id,
                        Drag {
                            index,
                            mode,
                            origin,
                            start_column: placement.column,
                            start_row: placement.row,
                            start_width: placement.width,
                            start_height: placement.height,
                        },
                    )
                });
                return Some(Edit::Select(Some(index)));
            }
        }

        let drag: Option<Drag> = ui.data(|data| data.get_temp(drag_id));
        let drag = drag.filter(|drag| drag.index == index)?;
        if !body.dragged() && !handle.dragged() && !body.drag_stopped() && !handle.drag_stopped() {
            return None;
        }
        let pointer = ui.ctx().pointer_interact_pos()?;
        let delta = pointer - drag.origin;
        let (columns, rows) = cells_for_delta(grid, delta, self.config.layout.snap_to_grid);

        let mut moved = placement.clone();
        match drag.mode {
            DragMode::Move => {
                moved.column = add_signed_u8(drag.start_column, columns, grid.columns);
                moved.row = add_signed_u16(drag.start_row, rows, MAX_LAYOUT_ROWS);
            }
            DragMode::Resize => {
                moved.width = add_signed_u8(drag.start_width, columns, grid.columns);
                moved.height = add_signed_u16(drag.start_height, rows, MAX_LAYOUT_ROWS);
            }
        }
        moved.clamp_to(grid);
        if body.drag_stopped() || handle.drag_stopped() {
            ui.data_mut(|data| data.remove::<Drag>(drag_id));
        }
        (moved != *placement).then_some(Edit::Replace(index, moved))
    }

    /// Floating toolbar: preset switching, grid size, and the add-widget palette.
    fn editor_toolbar(
        &mut self,
        ui: &mut egui::Ui,
        page: LayoutPage,
        selected: Option<usize>,
        placements: &[WidgetPlacement],
    ) -> Option<Edit> {
        let mut edit = None;
        let accent = self.accent_color();
        let can_add = placements.len() < MAX_WIDGET_PLACEMENTS;
        let mut grid = self.config.layout.active().grid;
        let mut grid_changed = false;
        let mut layout_flags = (
            self.config.layout.show_grid,
            self.config.layout.snap_to_grid,
            self.config.layout.edit_mode,
        );
        let mut flags_changed = false;
        let mut preset_request: Option<PresetRequest> = None;

        egui::Window::new("Layout editor")
            .id(egui::Id::new("layout_editor_toolbar"))
            .default_pos(ui.max_rect().left_top() + egui::vec2(24.0, 24.0))
            .default_width(320.0)
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(format!("Editing the {} page", page.label()))
                        .strong()
                        .color(accent),
                );
                ui.label(
                    RichText::new("Drag a widget to move it, drag its corner to resize.")
                        .small()
                        .color(MUTED),
                );
                ui.separator();

                preset_request = self.preset_controls(ui);

                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    grid_changed |= ui
                        .add(egui::Slider::new(&mut grid.columns, 1..=24).text("Columns"))
                        .changed();
                });
                ui.horizontal_wrapped(|ui| {
                    grid_changed |= ui
                        .add(egui::Slider::new(&mut grid.row_height, 24..=512).text("Row height"))
                        .changed();
                    grid_changed |= ui
                        .add(egui::Slider::new(&mut grid.gap, 0..=128).text("Gap"))
                        .changed();
                });
                ui.horizontal(|ui| {
                    flags_changed |= ui.checkbox(&mut layout_flags.0, "Show grid").changed();
                    flags_changed |= ui.checkbox(&mut layout_flags.1, "Snap").changed();
                });

                ui.separator();
                ui.label(RichText::new("ADD WIDGET").small().strong().color(MUTED));
                egui::ScrollArea::vertical()
                    .id_salt("layout_palette")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for group in WidgetGroup::ALL {
                            egui::CollapsingHeader::new(group.label())
                                .default_open(*group == WidgetGroup::Freeform)
                                .show(ui, |ui| {
                                    for registration in WIDGET_REGISTRY
                                        .iter()
                                        .filter(|entry| entry.group == *group)
                                    {
                                        if ui
                                            .add_enabled(
                                                can_add,
                                                egui::Button::new(registration.label),
                                            )
                                            .on_hover_text(registration.help)
                                            .clicked()
                                        {
                                            edit = Some(Edit::Add(registration.kind));
                                        }
                                    }
                                });
                        }
                    });

                if !placements
                    .iter()
                    .any(|placement| placement.enabled && is_body(&placement.widget))
                {
                    ui.separator();
                    ui.label(
                        RichText::new(
                            "This page has no page-body widget. Management controls for it are only reachable through buttons you place.",
                        )
                        .small()
                        .color(accent),
                    );
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Done").clicked() {
                        layout_flags.2 = false;
                        flags_changed = true;
                    }
                    if let Some(index) = selected {
                        if ui.button("Delete selected").clicked() {
                            edit = Some(Edit::Remove(index));
                        }
                    }
                });
            });

        if grid_changed {
            let preset = self.config.layout.active_mut();
            preset.grid = grid;
            for page_layout in all_pages_mut(preset) {
                for placement in &mut page_layout.placements {
                    placement.clamp_to(&grid);
                }
            }
            self.save_config_change("Saved grid size.");
        }
        if flags_changed {
            self.config.layout.show_grid = layout_flags.0;
            self.config.layout.snap_to_grid = layout_flags.1;
            self.config.layout.edit_mode = layout_flags.2;
            self.save_config_change("Saved layout editor settings.");
        }
        if let Some(request) = preset_request {
            self.apply_preset_request(request);
        }
        edit
    }

    /// Properties panel for the selected widget: content, style, order, enablement.
    fn widget_properties(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        placement: &WidgetPlacement,
    ) -> Option<Edit> {
        let mut draft = placement.clone();
        let mut changed = false;
        let mut edit = None;
        let columns = self.config.layout.active().grid.columns;

        egui::Window::new("Widget")
            .id(egui::Id::new("layout_widget_properties"))
            .default_pos(ui.max_rect().right_top() + egui::vec2(-320.0, 24.0))
            .default_width(280.0)
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new(widget_label(&draft.widget)).strong());
                changed |= ui.checkbox(&mut draft.enabled, "Visible").changed();

                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut draft.column)
                                .range(1..=columns.max(1))
                                .prefix("Col "),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut draft.row)
                                .range(1..=MAX_LAYOUT_ROWS)
                                .prefix("Row "),
                        )
                        .changed();
                });
                ui.horizontal_wrapped(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut draft.width)
                                .range(1..=columns.max(1))
                                .prefix("W "),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut draft.height)
                                .range(1..=MAX_LAYOUT_ROWS)
                                .prefix("H "),
                        )
                        .changed();
                });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Frame");
                    egui::ComboBox::from_id_salt("widget_frame")
                        .selected_text(draft.style.frame.label())
                        .show_ui(ui, |ui| {
                            for frame in WidgetFrame::ALL {
                                changed |= ui
                                    .selectable_value(&mut draft.style.frame, *frame, frame.label())
                                    .changed();
                            }
                        });
                });
                changed |= ui
                    .add(egui::Slider::new(&mut draft.style.padding, 0..=64).text("Padding"))
                    .changed();

                changed |= widget_content_ui(ui, &mut draft.widget);

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .small_button("Bring forward")
                        .on_hover_text("Overlapping widgets paint in list order")
                        .clicked()
                    {
                        edit = Some(Edit::Raise(index));
                    }
                    if ui.small_button("Send back").clicked() {
                        edit = Some(Edit::Lower(index));
                    }
                    if ui.small_button("Duplicate").clicked() {
                        edit = Some(Edit::Duplicate(index));
                    }
                    if ui.small_button("Delete").clicked() {
                        edit = Some(Edit::Remove(index));
                    }
                });
            });

        if edit.is_some() {
            return edit;
        }
        if changed {
            draft.clamp_to(&self.config.layout.active().grid);
            return Some(Edit::Replace(index, draft));
        }
        None
    }

    /// Commits one edit to the active preset and decides whether to touch the disk.
    fn apply_edit(&mut self, ui: &mut egui::Ui, page: LayoutPage, edit: Edit) {
        // A selection change is UI state, not configuration; it never hits the disk.
        if let Edit::Select(index) = edit {
            set_selected_index(ui, index);
            return;
        }

        let dragging = ui.ctx().dragged_id().is_some();
        let grid = self.config.layout.active().grid;
        let preset = self.config.layout.active_mut();
        let placements = &mut page_layout_mut(preset, page).placements;
        let message = match edit {
            Edit::Select(_) => unreachable!("handled above"),
            Edit::Add(kind) => {
                let registration = kind.registration();
                let mut placement = WidgetPlacement {
                    widget: kind.build(),
                    column: 1,
                    row: placements
                        .iter()
                        .map(WidgetPlacement::last_row)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1),
                    width: registration.default_width,
                    height: registration.default_height,
                    ..WidgetPlacement::default()
                };
                placement.clamp_to(&grid);
                placements.push(placement);
                set_selected_index(ui, Some(placements.len() - 1));
                "Added a widget."
            }
            Edit::Remove(index) if index < placements.len() => {
                placements.remove(index);
                set_selected_index(ui, None);
                "Removed a widget."
            }
            Edit::Duplicate(index) if index < placements.len() => {
                let mut copy = placements[index].clone();
                copy.row = copy.row.saturating_add(copy.height).min(MAX_LAYOUT_ROWS);
                copy.clamp_to(&grid);
                placements.push(copy);
                set_selected_index(ui, Some(placements.len() - 1));
                "Duplicated a widget."
            }
            Edit::Raise(index) if index + 1 < placements.len() => {
                placements.swap(index, index + 1);
                set_selected_index(ui, Some(index + 1));
                "Reordered widgets."
            }
            Edit::Lower(index) if index > 0 => {
                placements.swap(index, index - 1);
                set_selected_index(ui, Some(index - 1));
                "Reordered widgets."
            }
            Edit::Replace(index, placement) if index < placements.len() => {
                placements[index] = placement;
                "Saved layout."
            }
            _ => return,
        };

        // Persist only once the pointer is idle, so a drag writes the file once.
        if !dragging {
            self.save_config_change(message);
        }
    }

    // -- Presets ------------------------------------------------------------

    /// Preset switcher and save/duplicate/delete controls.
    fn preset_controls(&self, ui: &mut egui::Ui) -> Option<PresetRequest> {
        let mut request = None;
        let layout = &self.config.layout;
        ui.horizontal(|ui| {
            ui.label("Preset");
            egui::ComboBox::from_id_salt("layout_preset_picker")
                .selected_text(layout.active().name.clone())
                .show_ui(ui, |ui| {
                    for preset in &layout.presets {
                        if ui
                            .selectable_label(
                                preset.name == layout.active_preset,
                                preset.name.clone(),
                            )
                            .clicked()
                        {
                            request = Some(PresetRequest::Activate(preset.name.clone()));
                        }
                    }
                });
        });

        let name_id = ui.make_persistent_id("layout_preset_name");
        let mut name = ui
            .data_mut(|data| data.get_temp::<String>(name_id))
            .unwrap_or_else(|| "My layout".to_owned());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut name)
                    .desired_width(150.0)
                    .hint_text("Preset name"),
            );
            if ui
                .button("Save as")
                .on_hover_text("Copies the current layout under a new name")
                .clicked()
            {
                request = Some(PresetRequest::SaveAs(name.clone()));
            }
        });
        ui.data_mut(|data| data.insert_temp(name_id, name));

        ui.horizontal(|ui| {
            // The first preset is the shipped default and is never deletable, so
            // "reset to the original look" is always one click away.
            let deletable = layout.presets.len() > 1
                && layout
                    .presets
                    .first()
                    .is_some_and(|preset| preset.name != layout.active_preset);
            if ui
                .add_enabled(deletable, egui::Button::new("Delete preset"))
                .clicked()
            {
                request = Some(PresetRequest::Delete);
            }
            if ui
                .button("Reset page")
                .on_hover_text("Restores this page to the built-in default")
                .clicked()
            {
                request = Some(PresetRequest::ResetPage);
            }
            if ui.button("Add detailed preset").clicked() {
                request = Some(PresetRequest::AddDetailed);
            }
        });
        request
    }

    fn apply_preset_request(&mut self, request: PresetRequest) {
        let page = layout_page_from_page(self.current_page);
        let message = match request {
            PresetRequest::Activate(name) => {
                self.config.layout.active_preset = name;
                "Switched layout preset."
            }
            PresetRequest::SaveAs(name) => {
                let unique = self.config.layout.unique_preset_name(&name);
                let mut copy = self.config.layout.active().clone();
                copy.name = unique.clone();
                self.config.layout.presets.push(copy);
                self.config.layout.active_preset = unique;
                "Saved a new layout preset."
            }
            PresetRequest::Delete => {
                let active = self.config.layout.active_preset.clone();
                self.config.layout.presets.retain(|preset| preset.name != active);
                self.config.layout.ensure_invariants();
                "Deleted the layout preset."
            }
            PresetRequest::ResetPage => {
                let preset = self.config.layout.active_mut();
                *page_layout_mut(preset, page) = PageLayout::shell();
                "Restored the built-in page layout."
            }
            PresetRequest::AddDetailed => {
                let detailed = LayoutPreset::detailed();
                let name = self.config.layout.unique_preset_name(&detailed.name);
                self.config.layout.presets.push(LayoutPreset {
                    name: name.clone(),
                    ..detailed
                });
                self.config.layout.active_preset = name;
                "Added the detailed preset."
            }
        };
        self.save_config_change(message);
    }

    /// Layout section of the settings page: the on/off switches and the editor entry.
    pub(super) fn layout_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut enabled = self.config.layout.enabled;
        let mut edit_mode = self.config.layout.edit_mode;
        let mut changed = false;

        changed |= ui
            .checkbox(&mut enabled, "Use the customizable widget layout")
            .on_hover_text("When off, Ferrite draws its original fixed shell.")
            .changed();
        ui.label(
            RichText::new(
                "The default preset draws exactly the same UI as having this switched off.",
            )
            .small()
            .color(MUTED),
        );

        ui.add_space(8.0);
        ui.add_enabled_ui(enabled, |ui| {
            changed |= ui
                .checkbox(&mut edit_mode, "Edit layout")
                .on_hover_text("Shows the grid and lets you drag widgets around.")
                .changed();
        });

        if let Some(request) = self.preset_controls(ui) {
            self.apply_preset_request(request);
        }

        ui.add_space(8.0);
        ui.label(
            RichText::new(format!(
                "{} preset(s) saved · active: {}",
                self.config.layout.presets.len(),
                self.config.layout.active().name
            ))
            .small()
            .color(MUTED),
        );

        if changed {
            self.config.layout.enabled = enabled;
            self.config.layout.edit_mode = edit_mode && enabled;
            self.save_config_change("Saved layout settings.");
        }
    }
}

/// A preset operation requested from a UI closure and applied afterwards.
enum PresetRequest {
    Activate(String),
    SaveAs(String),
    Delete,
    ResetPage,
    AddDetailed,
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Returns the pixel rect a placement occupies inside `canvas`.
///
/// Columns divide the available width evenly after gaps are removed, so a layout keeps
/// its proportions when the window is resized. Rows are a fixed height, which is what
/// makes vertical position stable and the canvas scrollable.
pub(super) fn cell_rect(
    canvas: egui::Rect,
    grid: &GridConfig,
    placement: &WidgetPlacement,
) -> Option<egui::Rect> {
    if placement.column == 0
        || placement.row == 0
        || placement.width == 0
        || placement.height == 0
        || grid.columns == 0
    {
        return None;
    }
    let gap = f32::from(grid.gap);
    let usable = (canvas.width() - gap * f32::from(grid.columns.saturating_sub(1))).max(0.0);
    let column_width = usable / f32::from(grid.columns);
    let x = canvas.left() + f32::from(placement.column - 1) * (column_width + gap);
    let y = canvas.top() + f32::from(placement.row - 1) * (f32::from(grid.row_height) + gap);
    let width =
        f32::from(placement.width) * column_width + f32::from(placement.width - 1) * gap;
    let height = f32::from(placement.height) * f32::from(grid.row_height)
        + f32::from(placement.height - 1) * gap;
    Some(egui::Rect::from_min_size(
        egui::pos2(x, y),
        egui::vec2(width.max(0.0), height.max(0.0)),
    ))
}

/// Converts a pointer delta in pixels into a whole number of grid cells.
fn cells_for_delta(grid: &GridConfig, delta: egui::Vec2, snap: bool) -> (i32, i32) {
    let gap = f32::from(grid.gap);
    // Without a canvas width here, one column is approximated from the row height; the
    // caller re-clamps, and snapping means small errors cannot accumulate.
    let column_step = (f32::from(grid.row_height) + gap).max(1.0);
    let row_step = (f32::from(grid.row_height) + gap).max(1.0);
    let round = |value: f32| {
        if snap {
            value.round()
        } else {
            value.trunc()
        }
    };
    (
        round(delta.x / column_step) as i32,
        round(delta.y / row_step) as i32,
    )
}

fn add_signed_u8(base: u8, delta: i32, maximum: u8) -> u8 {
    (i32::from(base) + delta).clamp(1, i32::from(maximum.max(1))) as u8
}

fn add_signed_u16(base: u16, delta: i32, maximum: u16) -> u16 {
    (i64::from(base) + i64::from(delta)).clamp(1, i64::from(maximum)) as u16
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn selected_index(ui: &egui::Ui) -> Option<usize> {
    ui.data(|data| data.get_temp::<usize>(egui::Id::new("layout_selected_widget")))
}

fn set_selected_index(ui: &mut egui::Ui, index: Option<usize>) {
    let id = egui::Id::new("layout_selected_widget");
    ui.data_mut(|data| match index {
        Some(index) => data.insert_temp(id, index),
        None => data.remove::<usize>(id),
    });
}

fn page_layout(preset: &LayoutPreset, page: LayoutPage) -> &PageLayout {
    match page {
        LayoutPage::Play => &preset.play,
        LayoutPage::Instances => &preset.instances,
        LayoutPage::Mods => &preset.mods,
        LayoutPage::Settings => &preset.settings,
    }
}

fn page_layout_mut(preset: &mut LayoutPreset, page: LayoutPage) -> &mut PageLayout {
    match page {
        LayoutPage::Play => &mut preset.play,
        LayoutPage::Instances => &mut preset.instances,
        LayoutPage::Mods => &mut preset.mods,
        LayoutPage::Settings => &mut preset.settings,
    }
}

fn all_pages_mut(preset: &mut LayoutPreset) -> [&mut PageLayout; 4] {
    [
        &mut preset.play,
        &mut preset.instances,
        &mut preset.mods,
        &mut preset.settings,
    ]
}

/// Whether a widget can reach a page's own management controls.
fn is_body(widget: &Widget) -> bool {
    matches!(
        widget,
        Widget::PageBody | Widget::InstancesBody | Widget::ModsBody | Widget::SettingsBody
    )
}

/// Editors for the payload carried by text and button widgets.
fn widget_content_ui(ui: &mut egui::Ui, widget: &mut Widget) -> bool {
    let mut changed = false;
    match widget {
        Widget::Text { text } => {
            ui.separator();
            changed |= ui
                .add(
                    egui::TextEdit::multiline(text)
                        .hint_text("Text shown by this widget")
                        .desired_rows(3),
                )
                .changed();
            truncate_chars(text, MAX_WIDGET_TEXT_LENGTH);
        }
        Widget::ActionButton { label, action } => {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Label");
                changed |= ui.text_edit_singleline(label).changed();
            });
            truncate_chars(label, MAX_WIDGET_LABEL_LENGTH);
            ui.horizontal(|ui| {
                ui.label("Does");
                egui::ComboBox::from_id_salt("widget_action")
                    .selected_text(action_label(action))
                    .show_ui(ui, |ui| {
                        for (candidate, candidate_label) in ACTIONS {
                            changed |= ui
                                .selectable_value(action, candidate.clone(), *candidate_label)
                                .changed();
                        }
                    });
            });
        }
        Widget::NavButton { page } => {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Goes to");
                egui::ComboBox::from_id_salt("widget_nav_page")
                    .selected_text(page.label())
                    .show_ui(ui, |ui| {
                        for candidate in LayoutPage::ALL {
                            changed |=
                                ui.selectable_value(page, *candidate, candidate.label()).changed();
                        }
                    });
            });
        }
        _ => {}
    }
    changed
}

fn truncate_chars(value: &mut String, maximum: usize) {
    if value.chars().count() > maximum {
        *value = value.chars().take(maximum).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(column: u8, row: u16, width: u8, height: u16) -> WidgetPlacement {
        WidgetPlacement {
            column,
            row,
            width,
            height,
            ..WidgetPlacement::default()
        }
    }

    #[test]
    fn cell_rect_accounts_for_spans_and_gaps() {
        let grid = GridConfig {
            columns: 4,
            row_height: 20,
            gap: 10,
        };
        let rect = cell_rect(
            egui::Rect::from_min_size(egui::pos2(5.0, 7.0), egui::vec2(430.0, 200.0)),
            &grid,
            &placement(2, 2, 2, 3),
        )
        .unwrap();
        assert_eq!(rect.min, egui::pos2(115.0, 37.0));
        assert_eq!(rect.size(), egui::vec2(210.0, 80.0));
    }

    #[test]
    fn dragging_left_past_the_first_column_stops_at_one() {
        let grid = GridConfig::default();
        assert_eq!(add_signed_u8(2, -9, grid.columns), 1);
        assert_eq!(add_signed_u8(11, 9, grid.columns), 12);
    }

    #[test]
    fn resizing_below_one_row_is_refused() {
        assert_eq!(add_signed_u16(1, -5, MAX_LAYOUT_ROWS), 1);
        assert_eq!(add_signed_u16(999, 50, MAX_LAYOUT_ROWS), MAX_LAYOUT_ROWS);
    }

    #[test]
    fn snapping_rounds_while_free_drag_truncates() {
        let grid = GridConfig {
            row_height: 40,
            gap: 10,
            ..GridConfig::default()
        };
        assert_eq!(cells_for_delta(&grid, egui::vec2(0.0, 38.0), true).1, 1);
        assert_eq!(cells_for_delta(&grid, egui::vec2(0.0, 38.0), false).1, 0);
    }

    #[test]
    fn page_bodies_are_recognized_for_the_editor_warning() {
        assert!(is_body(&Widget::PageBody));
        assert!(is_body(&Widget::ModsBody));
        assert!(!is_body(&Widget::LaunchButton));
    }
}
