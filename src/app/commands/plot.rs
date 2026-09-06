//! Plot sheets: render the scene at an exact scale and compose a printable
//! drawing around it.

use anyhow::{Context, Result};
use glam::DVec3;

use crate::{
    app::App,
    model::plot::{self, LegendEntry, MapFrame, PlotLayout, PlotSpec, SheetInput, TextPainter},
    rendering::graphics::plot::PlotMapRequest,
    ui::dialogs::plot::{PlotCentre, PlotDialog},
    userspace_log, userspace_warn,
};

/// More than this many legend rows stops being a legend and starts being a
/// second drawing.
const MAX_LEGEND_ENTRIES: usize = 12;

impl<'a> App<'a> {
    pub(crate) fn open_plot_dialog(&mut self) {
        let mut dialog = self.editor.plot_dialog.clone().unwrap_or_default();
        if dialog.date.trim().is_empty() {
            dialog.date = chrono::Local::now().format("%Y-%m-%d").to_string();
        }
        if dialog.project_name.trim().is_empty()
            && let Some(project) = self.workspace.active_project()
            && let Some(stem) = project.path.as_ref().and_then(|path| path.file_stem())
        {
            dialog.project_name = stem.to_string_lossy().into_owned();
        }
        // Centre the entered coordinates on the data so switching to
        // "entered coordinates" starts somewhere useful.
        if let Some((min, max)) = self.visible_scene_extents() {
            let centre = (min + max) * 0.5;
            if dialog.centre_east == 0.0 && dialog.centre_north == 0.0 {
                dialog.centre_east = centre.x;
                dialog.centre_north = centre.y;
            }
        }
        self.editor.plot_dialog = Some(dialog);
    }

    /// World bounds of everything currently visible.
    fn visible_scene_extents(&self) -> Option<(DVec3, DVec3)> {
        self.graphics.as_ref()?.scene_extents(
            &self.scene_document,
            &self.triangulations,
            &self.block_models,
            &self.drill_holes,
            &self.point_clouds,
            &self.editor.hidden_handles,
        )
    }

    pub(crate) fn fit_plot_scale_to_data(&mut self) -> Result<()> {
        let Some(dialog) = self.editor.plot_dialog.clone() else {
            return Ok(());
        };
        let (min, max) = self.visible_scene_extents().context("There is nothing visible to fit the drawing to")?;
        let size = max - min;
        let spec = dialog.spec(Vec::new());
        // A 5 % margin keeps the data clear of the frame, matching how the
        // viewport's own fit behaves.
        let scale = plot::fitted_scale(&spec, size.x * 1.05, size.y * 1.05).map_err(|error| anyhow::anyhow!("{error}"))?;
        if let Some(dialog) = self.editor.plot_dialog.as_mut() {
            dialog.scale = scale;
            dialog.centre = PlotCentre::AllData;
        }
        userspace_log!(
            "{}",
            crate::i18n::tr_format!(
                literal = "Drawing scale fitted to visible data: 1:%scale%",
                scale = crate::model::plot::format_quantity(scale, 0)
            )
        );
        self.redraw_requested = true;
        Ok(())
    }

    /// Ask where the sheet should go; the render happens once a path is known
    /// so a cancelled dialog costs nothing.
    pub(crate) fn choose_plot_sheet_destination(&mut self) -> Result<()> {
        let dialog = self.editor.plot_dialog.clone().context("The engineering drawing dialog is not open")?;
        // Validate before opening a file chooser so bad settings surface now.
        plot::layout(&dialog.spec(Vec::new())).map_err(|error| anyhow::anyhow!("{error}"))?;
        let file_name = plot_file_name(&dialog);

        #[cfg(target_arch = "wasm32")]
        {
            self.write_plot_sheet(crate::app::commands::plot::PlotTarget::Browser(file_name))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.spawn_plot_sheet_dialog(file_name);
            Ok(())
        }
    }

    /// Render the map, compose the sheet and deliver the PNG.
    pub(crate) fn write_plot_sheet(&mut self, target: PlotTarget) -> Result<()> {
        let dialog = self.editor.plot_dialog.clone().context("The engineering drawing dialog is not open")?;
        let spec = dialog.spec(self.plot_legend_entries());
        let layout = plot::layout(&spec).map_err(|error| anyhow::anyhow!("{error}"))?;

        let centre = self.plot_centre(&dialog)?;
        // The projection zoom is a vertical half-extent, and the map's pixel
        // aspect already matches its world aspect, so this alone fixes both
        // axes at the requested scale.
        let zoom = layout.map.height * layout.world_per_px * 0.5;
        let request = PlotMapRequest {
            width: layout.map_width_px(),
            height: layout.map_height_px(),
            center: DVec3::new(centre.x, centre.y, centre.z),
            zoom,
        };
        let frame = MapFrame {
            center_east: centre.x,
            center_north: centre.y,
            world_per_px: layout.world_per_px,
        };

        let graphics = self.graphics.as_mut().context("The renderer is not initialised yet")?;
        let capture = graphics.capture_plot_map(
            request,
            &self.scene_document,
            &self.editor,
            &self.triangulations,
            &self.block_models,
            &self.drill_holes,
            &self.point_clouds,
            &self.raster_textures,
        )?;

        #[cfg(not(target_arch = "wasm32"))]
        {
            // The readback itself is a short GPU sync and has to happen here,
            // on the thread that owns the device. Everything after it is tens
            // of megapixels of CPU work, so it goes to a background job and
            // the window keeps responding while an A1 sheet is encoded.
            let pixels = graphics.resolve_plot_map(capture)?;
            self.editor.plot_dialog = None;
            self.redraw_requested = true;
            self.spawn_job_reporting_progress(
                crate::i18n::tr!(literal = "Composing engineering drawing…"),
                vec![crate::app::jobs::JobKey::Anonymous],
                move |cancel, progress| finish_plot_sheet(pixels, spec, layout, frame, target, cancel, progress),
                move |_app, result: Result<PlotSheetOutcome>| match result {
                    Ok(outcome) => outcome.log(),
                    Err(error) if error.to_string() == CANCELLED => {}
                    Err(error) => userspace_warn!(
                        "{}",
                        crate::i18n::tr_format!(literal = "Could not write the engineering drawing: %error%", error = format!("{error:#}"))
                    ),
                },
            );
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            // The browser maps the readback buffer asynchronously and hands it
            // back on the main thread; composing there keeps the delivery on
            // the one thread allowed to start a download.
            graphics.resolve_plot_map_async(capture, move |pixels| {
                // Nothing samples either of these here: the compose runs inline
                // on the main thread, so there is no worker to cancel and no
                // frame in between to draw a bar from.
                let outcome = pixels.and_then(|pixels| {
                    finish_plot_sheet(
                        pixels,
                        spec,
                        layout,
                        frame,
                        target,
                        &crate::app::jobs::CancelFlag::default(),
                        &crate::model::progress::Progress::new(),
                    )
                });
                match outcome {
                    Ok(outcome) => outcome.log(),
                    Err(error) => userspace_warn!(
                        "{}",
                        crate::i18n::tr_format!(literal = "Could not write the engineering drawing: %error%", error = format!("{error:#}"))
                    ),
                }
            });
            self.editor.plot_dialog = None;
            self.redraw_requested = true;
            Ok(())
        }
    }

    fn plot_centre(&self, dialog: &PlotDialog) -> Result<DVec3> {
        match dialog.centre {
            PlotCentre::Explicit => Ok(DVec3::new(dialog.centre_east, dialog.centre_north, self.plot_reference_elevation())),
            PlotCentre::CurrentView => {
                let graphics = self.graphics.as_ref().context("The renderer is not initialised yet")?;
                Ok(graphics.view_center())
            }
            PlotCentre::AllData => {
                let (min, max) = self.visible_scene_extents().context("There is nothing visible to draw")?;
                Ok((min + max) * 0.5)
            }
        }
    }

    /// Mid-elevation of the visible scene; a plan view only needs a sensible
    /// focal depth, not an exact one.
    fn plot_reference_elevation(&self) -> f64 {
        self.visible_scene_extents().map_or(0.0, |(min, max)| (min.z + max.z) * 0.5)
    }

    /// Legend rows for the visible surfaces and the active project's loaded
    /// design layers.
    fn plot_legend_entries(&self) -> Vec<LegendEntry> {
        let mut entries: Vec<LegendEntry> = self
            .triangulations
            .iter()
            .filter(|tri| tri.state.loaded && !self.editor.hidden_handles.contains(&tri.entity_id()))
            .map(|tri| LegendEntry {
                label: tri.name.clone(),
                color: tri.color,
            })
            .collect();
        if let Some(project) = self.workspace.active_project() {
            entries.extend(project.project.document.layers().iter().filter(|layer| layer.loaded).map(|layer| LegendEntry {
                label: layer.name.clone(),
                color: layer.color,
            }));
        }
        entries.truncate(MAX_LEGEND_ENTRIES);
        entries
    }
}

/// Where a finished sheet is delivered.
#[derive(Clone, Debug)]
pub(crate) enum PlotTarget {
    #[cfg(not(target_arch = "wasm32"))]
    File(std::path::PathBuf),
    #[cfg(target_arch = "wasm32")]
    Browser(String),
}

/// Error text the job layer treats as "the user withdrew this", not a failure.
const CANCELLED: &str = "Cancelled";

/// What a finished sheet turned into, so the console line is written on the
/// main thread rather than from a worker.
#[derive(Debug)]
pub(crate) struct PlotSheetOutcome {
    description: String,
    width: u32,
    height: u32,
    dpi: u32,
}

impl PlotSheetOutcome {
    fn log(&self) {
        userspace_log!(
            "{}",
            crate::i18n::tr_format!(
                literal = "Saved engineering drawing: %description% (%width% × %height% px at %dpi% dpi)",
                description = &self.description,
                width = self.width,
                height = self.height,
                dpi = self.dpi
            )
        );
    }
}

/// Compose the sheet around the rendered map and write it out.
///
/// This is the expensive half of an export - a paper-sized canvas, glyph
/// rasterisation and PNG deflate, tens of megapixels at print resolution - so
/// it owns everything it needs and runs on a worker wherever the platform
/// allows one.
fn finish_plot_sheet(
    map_pixels: Vec<u8>,
    spec: PlotSpec,
    layout: PlotLayout,
    frame: MapFrame,
    target: PlotTarget,
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Progress,
) -> Result<PlotSheetOutcome> {
    let dpi = spec.dpi;
    if cancel.is_cancelled() {
        anyhow::bail!(CANCELLED);
    }
    let mut painter = TextPainter::new();
    let canvas = plot::compose_sheet(
        SheetInput {
            spec,
            layout,
            frame,
            map_pixels: &map_pixels,
        },
        &mut painter,
    );
    if cancel.is_cancelled() {
        anyhow::bail!(CANCELLED);
    }
    // Compose, encode, write: three single calls over the whole sheet, so the
    // bar steps between them on an estimate of their relative cost.
    progress.set_fraction(0.5);
    let bytes = plot::encode_png(&canvas, dpi).map_err(|error| anyhow::anyhow!("Could not encode the sheet: {error}"))?;
    progress.set_fraction(0.9);

    let description = match target {
        #[cfg(not(target_arch = "wasm32"))]
        PlotTarget::File(path) => {
            std::fs::write(&path, &bytes).with_context(|| format!("Could not save {}", path.display()))?;
            path.display().to_string()
        }
        #[cfg(target_arch = "wasm32")]
        PlotTarget::Browser(file_name) => {
            crate::app::web_download::download(&file_name, &bytes, "image/png").map_err(|error| anyhow::anyhow!("Download failed: {error}"))?;
            file_name
        }
    };
    Ok(PlotSheetOutcome {
        description,
        width: layout.sheet_width_px,
        height: layout.sheet_height_px,
        dpi,
    })
}

/// Turn a title or drawing number into a safe file stem.
fn sanitised_file_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|character| if character.is_alphanumeric() { character.to_ascii_lowercase() } else { '_' })
        .collect();
    let trimmed = stem.trim_matches('_').to_owned();
    if trimmed.is_empty() { crate::i18n::tr!(literal = "Plot") } else { trimmed }
}

/// Default file name: the drawing number if there is one, otherwise the title.
pub(crate) fn plot_file_name(dialog: &PlotDialog) -> String {
    let stem = if dialog.drawing_number.trim().is_empty() {
        dialog.title.trim()
    } else {
        dialog.drawing_number.trim()
    };
    let fallback = crate::i18n::tr!(literal = "Plot");
    let stem = sanitised_file_stem(if stem.is_empty() { &fallback } else { stem });
    format!("{stem}.png")
}
