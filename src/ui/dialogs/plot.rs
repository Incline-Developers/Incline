//! Engineering drawing export dialog: paper, scale, sheet furniture and a
//! live layout preview.

use strum::IntoEnumIterator;

use crate::{
    model::plot::{LegendEntry, Orientation, PaperSize, PlotLayout, PlotSpec, RectPx, SheetFurniture, TitleBlock, furniture, layout, max_dpi_for, sheet_furniture},
    ui::{
        state::{EditorState, UiCommand, UiProjectView},
        widgets::menu::{DragableMenu, MenuFieldBool, MenuFieldCombo, MenuFieldF64, MenuFieldText, MenuFieldU32},
    },
};

const PLOT_SETTINGS_WIDTH: f32 = 470.0;
const PLOT_PREVIEW_WIDTH: f32 = 250.0;
const PLOT_DIALOG_WIDTH: f32 = PLOT_SETTINGS_WIDTH + PLOT_PREVIEW_WIDTH + 24.0;
const PLOT_FIELD_WIDTH: f32 = 250.0;
/// Keeps the settings column from growing taller than a laptop screen.
const PLOT_SETTINGS_MAX_HEIGHT: f32 = 560.0;

/// The preview is drawn as paper, so its ink does not follow the app theme.
const PREVIEW_PAPER: egui::Color32 = egui::Color32::from_gray(252);
const PREVIEW_INK: egui::Color32 = egui::Color32::from_gray(40);
const PREVIEW_FAINT_INK: egui::Color32 = egui::Color32::from_gray(155);
const PREVIEW_MAP_FILL: egui::Color32 = egui::Color32::from_rgb(232, 237, 241);

/// Where the plot is centred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumIter, strum::Display)]
pub(crate) enum PlotCentre {
    #[strum(to_string = "All visible data")]
    AllData,
    #[strum(to_string = "Current view centre")]
    CurrentView,
    #[strum(to_string = "Entered coordinates")]
    Explicit,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlotDialog {
    pub(crate) paper: PaperSize,
    pub(crate) orientation: Orientation,
    pub(crate) dpi: u32,
    pub(crate) margin_mm: f64,
    pub(crate) scale: f64,
    pub(crate) centre: PlotCentre,
    pub(crate) centre_east: f64,
    pub(crate) centre_north: f64,
    pub(crate) show_frame: bool,
    pub(crate) show_grid: bool,
    pub(crate) auto_grid: bool,
    pub(crate) grid_interval: f64,
    pub(crate) show_scale_bar: bool,
    pub(crate) show_north_arrow: bool,
    pub(crate) show_legend: bool,
    pub(crate) show_title_block: bool,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) project_name: String,
    pub(crate) author: String,
    pub(crate) drawing_number: String,
    pub(crate) revision: String,
    pub(crate) date: String,
}

impl Default for PlotDialog {
    fn default() -> Self {
        Self {
            paper: PaperSize::A3,
            orientation: Orientation::Landscape,
            dpi: 300,
            margin_mm: 12.0,
            scale: 1000.0,
            centre: PlotCentre::AllData,
            centre_east: 0.0,
            centre_north: 0.0,
            show_frame: true,
            show_grid: true,
            auto_grid: true,
            grid_interval: 100.0,
            show_scale_bar: true,
            show_north_arrow: true,
            show_legend: true,
            show_title_block: true,
            title: "Plan".to_owned(),
            subtitle: String::new(),
            project_name: String::new(),
            author: String::new(),
            drawing_number: String::new(),
            revision: "A".to_owned(),
            date: String::new(),
        }
    }
}

impl PlotDialog {
    pub(crate) fn spec(&self, legend: Vec<LegendEntry>) -> PlotSpec {
        PlotSpec {
            paper: self.paper,
            orientation: self.orientation,
            dpi: self.dpi,
            margin_mm: self.margin_mm,
            scale: self.scale,
            title_block: self.show_title_block.then(|| TitleBlock {
                title: self.title.clone(),
                subtitle: self.subtitle.clone(),
                project: self.project_name.clone(),
                author: self.author.clone(),
                drawing_number: self.drawing_number.clone(),
                revision: self.revision.clone(),
                date: self.date.clone(),
            }),
            show_frame: self.show_frame,
            show_grid: self.show_grid,
            grid_interval: (!self.auto_grid).then_some(self.grid_interval),
            show_scale_bar: self.show_scale_bar,
            show_north_arrow: self.show_north_arrow,
            legend: if self.show_legend { legend } else { Vec::new() },
        }
    }
}

/// A scale-accurate preview of the sheet the export will produce.
///
/// The map is the real scene, rendered by the GPU at the drawing's own
/// framing and scale, so what is inside the frame is what will be on the
/// paper. The furniture around it is drawn to its true position and size, but
/// its text is suggested with rules rather than typeset, because at thumbnail
/// size real labels would be one or two pixels tall and only add noise.
fn draw_sheet_preview(ui: &mut egui::Ui, dialog: &PlotDialog, legend_entries: &[LegendEntry], map_texture: Option<egui::TextureId>) {
    let spec = dialog.spec(legend_entries.to_vec());
    let sheet = match layout(&spec) {
        Ok(sheet) => sheet,
        Err(error) => {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
            return;
        }
    };

    let aspect = f64::from(sheet.sheet_height_px) / f64::from(sheet.sheet_width_px).max(1.0);
    let width = PLOT_PREVIEW_WIDTH;
    let (response, painter) = ui.allocate_painter(egui::vec2(width, width * aspect as f32), egui::Sense::hover());
    let paper = response.rect;
    // Sheet pixels to preview points.
    let scale = paper.width() as f64 / f64::from(sheet.sheet_width_px).max(1.0);
    let to_preview = |rect: RectPx| {
        egui::Rect::from_min_size(
            paper.min + egui::vec2((rect.x * scale) as f32, (rect.y * scale) as f32),
            egui::vec2((rect.width * scale) as f32, (rect.height * scale) as f32),
        )
    };

    painter.rect_filled(paper, 1.0, PREVIEW_PAPER);
    painter.rect_stroke(paper, 1.0, egui::Stroke::new(1.0, PREVIEW_FAINT_INK), egui::StrokeKind::Inside);

    // The map is the scene rendered at the drawing's own framing and scale,
    // so the preview shows what will actually be on the paper.
    let map = to_preview(sheet.map);
    match map_texture {
        Some(texture) => {
            painter.image(texture, map, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        None => {
            painter.rect_filled(map, 0.0, PREVIEW_MAP_FILL);
            painter.text(
                map.center(),
                egui::Align2::CENTER_CENTER,
                "Nothing visible to draw",
                egui::FontId::proportional(11.0),
                PREVIEW_FAINT_INK,
            );
        }
    }

    // Estimate the widest legend label without shaping any text; only this
    // box's width depends on it, and a thumbnail can absorb the error.
    let legend_label_px = legend_entries
        .iter()
        .map(|entry| estimated_text_width(&entry.label, furniture::LEGEND_LABEL_MM * sheet.px_per_mm))
        .fold(estimated_text_width("Legend", furniture::LEGEND_TITLE_MM * sheet.px_per_mm), f64::max)
        .min(sheet.map.width * 0.25);
    let placement = sheet_furniture(&spec, &sheet, legend_label_px, legend_entries.len());

    if let Some(interval) = placement.grid_interval {
        draw_preview_grid(&painter, map, &sheet, interval);
    }
    if spec.show_frame {
        painter.rect_stroke(map, 0.0, egui::Stroke::new(1.0, PREVIEW_INK), egui::StrokeKind::Inside);
    }
    draw_preview_furniture(&painter, &placement, legend_entries.len(), to_preview);

    ui.add_space(6.0);
    let (world_width, world_height) = sheet.map_world_size();
    let (paper_width_mm, paper_height_mm) = {
        let (short, long) = dialog.paper.portrait_mm();
        match dialog.orientation {
            Orientation::Portrait => (short, long),
            Orientation::Landscape => (long, short),
        }
    };
    ui.label(format!(
        "{} {} · {paper_width_mm:.0} × {paper_height_mm:.0} mm",
        dialog.paper,
        dialog.orientation.to_string().to_lowercase(),
    ));
    ui.weak(format!(
        "1:{} · covers {} × {} m",
        crate::model::plot::format_quantity(dialog.scale, 0),
        crate::model::plot::format_quantity(world_width, 0),
        crate::model::plot::format_quantity(world_height, 0),
    ));
    ui.weak(format!("{} × {} px at {} dpi", sheet.sheet_width_px, sheet.sheet_height_px, dialog.dpi));
}

/// Rough rendered width of a string, used only where a shaped measurement
/// would cost more than the result is worth.
fn estimated_text_width(text: &str, size_px: f64) -> f64 {
    text.chars().count() as f64 * size_px * 0.55
}

fn draw_preview_grid(painter: &egui::Painter, map: egui::Rect, sheet: &PlotLayout, interval: f64) {
    let spacing = (interval / sheet.world_per_px * (map.width() as f64 / sheet.map.width)) as f32;
    if spacing < 3.0 {
        return;
    }
    let stroke = egui::Stroke::new(1.0, PREVIEW_FAINT_INK.gamma_multiply(0.75));
    let mut x = map.left() + spacing;
    while x < map.right() {
        painter.line_segment([egui::pos2(x, map.top()), egui::pos2(x, map.bottom())], stroke);
        x += spacing;
    }
    let mut y = map.top() + spacing;
    while y < map.bottom() {
        painter.line_segment([egui::pos2(map.left(), y), egui::pos2(map.right(), y)], stroke);
        y += spacing;
    }
}

fn draw_preview_furniture(painter: &egui::Painter, placement: &SheetFurniture, legend_rows: usize, to_preview: impl Fn(RectPx) -> egui::Rect) {
    let box_stroke = egui::Stroke::new(1.0, PREVIEW_INK);
    let rule = |rect: egui::Rect, y: f32, from: f32, to: f32, thickness: f32, color: egui::Color32| {
        if rect.width() > 6.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(rect.left() + from, y), egui::vec2((to - from).max(1.0), thickness)),
                0.0,
                color,
            );
        }
    };

    if let Some(rect) = placement.legend.map(&to_preview) {
        painter.rect_filled(rect, 0.0, PREVIEW_PAPER);
        painter.rect_stroke(rect, 0.0, box_stroke, egui::StrokeKind::Inside);
        let row_height = (rect.height() - 4.0) / (legend_rows.max(1) + 1) as f32;
        for row in 0..legend_rows {
            let y = rect.top() + 2.0 + row_height * (row + 1) as f32;
            let swatch = egui::Rect::from_min_size(egui::pos2(rect.left() + 2.0, y), egui::vec2(row_height.min(3.5), row_height.min(3.5)));
            painter.rect_filled(swatch, 0.0, PREVIEW_FAINT_INK);
            rule(rect, y + 0.5, swatch.width() + 4.0, rect.width() - 2.0, 1.0, PREVIEW_FAINT_INK);
        }
    }

    if let Some(rect) = placement.scale_bar.map(&to_preview) {
        painter.rect_filled(rect, 0.0, PREVIEW_PAPER);
        painter.rect_stroke(rect, 0.0, box_stroke, egui::StrokeKind::Inside);
        // The bar itself: four alternating segments across the middle band.
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 3.0, rect.top() + rect.height() * 0.38),
            egui::pos2(rect.right() - 3.0, rect.top() + rect.height() * 0.62),
        );
        if bar.width() > 4.0 {
            let segment = bar.width() / 4.0;
            for index in 0..4 {
                let cell = egui::Rect::from_min_size(egui::pos2(bar.left() + segment * index as f32, bar.top()), egui::vec2(segment, bar.height()));
                painter.rect_filled(cell, 0.0, if index % 2 == 0 { PREVIEW_INK } else { PREVIEW_PAPER });
                painter.rect_stroke(cell, 0.0, egui::Stroke::new(0.5, PREVIEW_INK), egui::StrokeKind::Inside);
            }
        }
    }

    if let Some(rect) = placement.north_arrow.map(&to_preview) {
        painter.rect_filled(rect, 0.0, PREVIEW_PAPER);
        painter.rect_stroke(rect, 0.0, box_stroke, egui::StrokeKind::Inside);
        let centre_x = rect.center().x;
        let top = rect.top() + rect.height() * 0.1;
        let bottom = rect.top() + rect.height() * 0.68;
        painter.add(egui::Shape::convex_polygon(
            vec![egui::pos2(centre_x, top), egui::pos2(centre_x, bottom), egui::pos2(centre_x - rect.width() * 0.2, bottom)],
            PREVIEW_INK,
            egui::Stroke::NONE,
        ));
        painter.add(egui::Shape::convex_polygon(
            vec![egui::pos2(centre_x, top), egui::pos2(centre_x + rect.width() * 0.2, bottom), egui::pos2(centre_x, bottom)],
            PREVIEW_PAPER,
            egui::Stroke::new(0.5, PREVIEW_INK),
        ));
        if rect.height() > 18.0 {
            painter.text(
                egui::pos2(centre_x, bottom + 1.0),
                egui::Align2::CENTER_TOP,
                "N",
                egui::FontId::proportional((rect.height() * 0.22).clamp(6.0, 11.0)),
                PREVIEW_INK,
            );
        }
    }

    if let Some(rect) = placement.title_block.map(&to_preview) {
        painter.rect_filled(rect, 0.0, PREVIEW_PAPER);
        painter.rect_stroke(rect, 0.0, box_stroke, egui::StrokeKind::Inside);
        // Title, subtitle and project lines, then the five-column field strip.
        rule(rect, rect.top() + 3.0, 3.0, rect.width() * 0.62, 2.5, PREVIEW_INK);
        rule(rect, rect.top() + 8.0, 3.0, rect.width() * 0.45, 1.0, PREVIEW_FAINT_INK);
        rule(rect, rect.top() + 12.0, 3.0, rect.width() * 0.4, 1.0, PREVIEW_FAINT_INK);
        let strip_y = rect.bottom() - (rect.height() * 0.3).max(6.0);
        painter.line_segment([egui::pos2(rect.left(), strip_y), egui::pos2(rect.right(), strip_y)], box_stroke);
        for index in 1..5 {
            let x = rect.left() + rect.width() * index as f32 / 5.0;
            painter.line_segment([egui::pos2(x, strip_y), egui::pos2(x, rect.bottom())], egui::Stroke::new(0.5, PREVIEW_INK));
        }
    }
}

/// Legend rows the export will produce, mirroring `App::plot_legend_entries`
/// so the preview's legend box is the size the sheet will actually get.
fn preview_legend_entries(project: &UiProjectView) -> Vec<LegendEntry> {
    const MAX_LEGEND_ENTRIES: usize = 12;
    let mut entries: Vec<LegendEntry> = project
        .triangulations
        .iter()
        .filter(|entry| entry.is_loaded && entry.visible)
        .map(|entry| LegendEntry {
            label: entry.name.clone(),
            color: [0.5, 0.5, 0.5, 1.0],
        })
        .collect();
    if let Some(active) = project.projects.iter().find(|entry| entry.is_active) {
        entries.extend(active.layers.iter().filter(|layer| layer.is_loaded).map(|layer| LegendEntry {
            label: layer.name.clone(),
            color: [0.5, 0.5, 0.5, 1.0],
        }));
    }
    entries.truncate(MAX_LEGEND_ENTRIES);
    entries
}

pub(crate) fn draw_plot_dialog(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, commands: &mut Vec<UiCommand>) {
    let Some(mut dialog) = editor.plot_dialog.clone() else {
        return;
    };
    let mut open = true;
    let mut cancel = false;
    let mut export = false;
    let legend_entries = preview_legend_entries(project);

    DragableMenu::new("Export Engineering Drawing")
        .open(&mut open)
        .min_width(PLOT_DIALOG_WIDTH)
        .max_width(PLOT_DIALOG_WIDTH + 40.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui.ctx(), |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(PLOT_PREVIEW_WIDTH);
                    draw_sheet_preview(ui, &dialog, &legend_entries, editor.plot_preview_texture);
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(PLOT_SETTINGS_WIDTH);
                    egui::ScrollArea::vertical()
                        .id_salt("plot_settings")
                        .max_height(PLOT_SETTINGS_MAX_HEIGHT)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // ── Paper ──
                            let paper_label = dialog.paper.to_string();
                            MenuFieldCombo::new(
                                "plot_paper",
                                "Paper size",
                                &mut dialog.paper,
                                paper_label,
                                PaperSize::iter().map(|size| {
                                    let (width, height) = size.portrait_mm();
                                    (size, format!("{size} ({width:.0} × {height:.0} mm)").into())
                                }),
                            )
                            .width(PLOT_FIELD_WIDTH)
                            .show(ui);

                            let orientation_label = dialog.orientation.to_string();
                            MenuFieldCombo::new(
                                "plot_orientation",
                                "Orientation",
                                &mut dialog.orientation,
                                orientation_label,
                                Orientation::iter().map(|orientation| (orientation, orientation.to_string().into())),
                            )
                            .width(PLOT_FIELD_WIDTH)
                            .show(ui);

                            let max_dpi = max_dpi_for(dialog.paper, dialog.orientation);
                            dialog.dpi = dialog.dpi.min(max_dpi);
                            MenuFieldU32::new("Resolution", &mut dialog.dpi, 72..=max_dpi)
                                .help_text(format!(
                                    "Dots per inch. This paper size can be rasterised up to {max_dpi} dpi; 300 dpi is a normal print quality."
                                ))
                                .suffix(" dpi")
                                .speed(5.0)
                                .width(PLOT_FIELD_WIDTH)
                                .show(ui);

                            MenuFieldF64::new("Margin", &mut dialog.margin_mm, 0.0..=60.0)
                                .suffix(" mm")
                                .speed(0.5)
                                .max_decimals(1)
                                .width(PLOT_FIELD_WIDTH)
                                .show(ui);

                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(2.0);

                            // ── Scale and framing ──
                            MenuFieldF64::new("Scale  1:", &mut dialog.scale, 1.0..=1.0e7)
                                .help_text("At 1:1000, one millimetre on the sheet is one metre on the ground.")
                                .speed(10.0)
                                .max_decimals(0)
                                .width(PLOT_FIELD_WIDTH)
                                .show(ui);

                            let centre_label = dialog.centre.to_string();
                            MenuFieldCombo::new(
                                "plot_centre",
                                "Centre on",
                                &mut dialog.centre,
                                centre_label,
                                PlotCentre::iter().map(|centre| (centre, centre.to_string().into())),
                            )
                            .width(PLOT_FIELD_WIDTH)
                            .show(ui);
                            if dialog.centre == PlotCentre::Explicit {
                                MenuFieldF64::new("Easting", &mut dialog.centre_east, f64::MIN..=f64::MAX)
                                    .speed(1.0)
                                    .max_decimals(3)
                                    .width(PLOT_FIELD_WIDTH)
                                    .show(ui);
                                MenuFieldF64::new("Northing", &mut dialog.centre_north, f64::MIN..=f64::MAX)
                                    .speed(1.0)
                                    .max_decimals(3)
                                    .width(PLOT_FIELD_WIDTH)
                                    .show(ui);
                            }

                            if ui
                                .button("Fit scale to visible data")
                                .on_hover_text("Choose the smallest conventional scale that fits everything visible onto the sheet.")
                                .clicked()
                            {
                                commands.push(UiCommand::FitPlotScaleToData);
                            }

                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(2.0);

                            // ── Sheet furniture ──
                            MenuFieldBool::new("Border", &mut dialog.show_frame).show(ui);
                            MenuFieldBool::new("Coordinate grid", &mut dialog.show_grid).show(ui);
                            if dialog.show_grid {
                                MenuFieldBool::new("Automatic grid interval", &mut dialog.auto_grid)
                                    .help_text("Pick an interval that reads roughly every 50 mm on the printed sheet.")
                                    .show(ui);
                                if !dialog.auto_grid {
                                    MenuFieldF64::new("Grid interval", &mut dialog.grid_interval, 0.001..=1.0e6)
                                        .suffix(" m")
                                        .speed(1.0)
                                        .max_decimals(2)
                                        .width(PLOT_FIELD_WIDTH)
                                        .show(ui);
                                }
                            }
                            MenuFieldBool::new("Scale bar", &mut dialog.show_scale_bar).show(ui);
                            MenuFieldBool::new("North arrow", &mut dialog.show_north_arrow).show(ui);
                            MenuFieldBool::new("Legend", &mut dialog.show_legend)
                                .help_text("Lists the visible surfaces and design layers with their colours.")
                                .show(ui);

                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(2.0);

                            // ── Title block ──
                            MenuFieldBool::new("Title block", &mut dialog.show_title_block).show(ui);
                            if dialog.show_title_block {
                                MenuFieldText::new("Title", &mut dialog.title).width(PLOT_FIELD_WIDTH).show(ui);
                                MenuFieldText::new("Subtitle", &mut dialog.subtitle).width(PLOT_FIELD_WIDTH).show(ui);
                                MenuFieldText::new("Project", &mut dialog.project_name)
                                    .width(PLOT_FIELD_WIDTH)
                                    .hint_text(
                                        project
                                            .active_path
                                            .as_ref()
                                            .and_then(|path| path.file_stem())
                                            .map_or_else(|| "e.g. Example Gold Project".to_owned(), |stem| stem.to_string_lossy().into_owned()),
                                    )
                                    .show(ui);
                                MenuFieldText::new("Drawn by", &mut dialog.author).width(PLOT_FIELD_WIDTH).show(ui);
                                MenuFieldText::new("Date", &mut dialog.date).width(PLOT_FIELD_WIDTH).hint_text("today").show(ui);
                                MenuFieldText::new("Drawing number", &mut dialog.drawing_number).width(PLOT_FIELD_WIDTH).show(ui);
                                MenuFieldText::new("Revision", &mut dialog.revision).width(PLOT_FIELD_WIDTH).show(ui);
                            }
                        });

                    ui.add_space(6.0);
                    ui.separator();
                    let valid = layout(&dialog.spec(Vec::new())).is_ok();
                    ui.horizontal(|ui| {
                        export = ui.add_enabled(valid, egui::Button::new("Export PNG...")).clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                    ui.weak("The PNG is written at the sheet's exact paper size and records its DPI, so it prints at true scale.");
                });
            });
        });

    if export {
        commands.push(UiCommand::ExportPlotSheet);
    }
    editor.plot_dialog = (open && !cancel).then_some(dialog);
}
