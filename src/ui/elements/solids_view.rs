//! The Solids page's View subpage: everything the setup has generated.
//!
//! Where the Setup pages configure a solid, this one is for looking through
//! what came out - the solids grouped by what they are, each opening into its
//! benches and those into their flitches - and reading off what any selection
//! of them holds.

use crate::{
    i18n::tr,
    model::{Document, SolidKind},
    ui::{
        EditorState, UiProjectView, chrome,
        dialogs::solids::{block_model_label, kind_label},
        fonts::bold,
        state::{BenchSelection, SolidsViewRow, UiCommand},
        unthemed_icon,
        widgets::{
            data_grid::{PropertyTable, property_table_height},
            explorer::{ExplorerEntry, ExplorerHeader, explorer_note, paint_fixed_stripes, reserve_fixed_stripes},
        },
    },
};

/// Draw the tree: kind, solid, bench, blast, then flitch.
///
/// Rows are selected the way a file list is - click to replace the selection,
/// ctrl-click to add to it - because the properties beside the tree are about
/// however many rows are picked, not just one. Clicking a parent picks
/// everything under it, so the arrows are what collapse a row rather than its
/// label; flitches start collapsed, since a pit has tens of them and the
/// benches are what the tree is read for.
pub(crate) fn draw_tree(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document) {
    draw_tree_to_depth(ui, editor, document, true);
}

/// The same tree stopping at benches, for the Blasting step: a blast divides
/// a bench, and flitches have nothing to say about it.
pub(crate) fn draw_bench_tree(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document) {
    draw_tree_to_depth(ui, editor, document, false);
}

/// Dig Strips chooses a whole flitch, independently of blast partitions.
///
/// Only the ground the solid actually occupies is offered: a bench or solid
/// the benching plan names but the body never reaches has no flitch to draw
/// strips on, so it is left out rather than opening onto nothing.
pub(crate) fn draw_flitch_tree(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document) {
    // Banded and full-height like every other tree: its island is a region of
    // its own, so a short list has to fill it rather than shrink it away.
    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
    egui::ScrollArea::vertical().auto_shrink([false; 2]).min_scrolled_height(0.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let (slot, top) = reserve_fixed_stripes(ui);
        if document.solids().is_empty() {
            explorer_note(ui, tr!(literal = "No solids yet - add one on the Setup page"));
        }
        for solid in document.solids() {
            let occupied = editor.solid_view_bands.get(&solid.id);
            let benches: Vec<_> = solid
                .benching
                .benches()
                .into_iter()
                .rev()
                .filter(|bench| holds(occupied, bench.base, bench.top()))
                .collect();
            if benches.is_empty() && occupied.is_some() {
                continue;
            }
            egui::CollapsingHeader::new(&solid.name).id_salt(("strip_solid", solid.id)).show(ui, |ui| {
                if occupied.is_none() {
                    explorer_note(ui, tr!("planning-solid-geometry-pending"));
                }
                for bench in &benches {
                    egui::CollapsingHeader::new(format_rl(bench.base))
                        .id_salt(("strip_bench", solid.id, bench.base.to_bits()))
                        .show(ui, |ui| {
                            for flitch in bench.flitches.iter().rev().filter(|flitch| holds(occupied, flitch.base, flitch.top())) {
                                let row = SolidsViewRow {
                                    solid: solid.id,
                                    band: Some(BenchSelection {
                                        base: flitch.base,
                                        top: flitch.top(),
                                        is_flitch: true,
                                    }),
                                };
                                if leaf_row(
                                    ui,
                                    egui::Id::new(("strip_flitch", solid.id, flitch.base.to_bits())),
                                    &format_rl(flitch.base),
                                    editor.solids_view_selection == [row],
                                ) {
                                    editor.solids_view_selection = vec![row];
                                    editor.selected_blast = None;
                                    ui.ctx().request_repaint();
                                }
                            }
                        });
                }
            });
        }
        paint_fixed_stripes(ui, slot, top, crate::ui::widgets::tree_row_colors(ui).1);
    });
}

fn draw_tree_to_depth(ui: &mut egui::Ui, editor: &mut EditorState, document: &Document, show_flitches: bool) {
    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
    egui::ScrollArea::vertical().auto_shrink([false; 2]).min_scrolled_height(0.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let (slot, top) = reserve_fixed_stripes(ui);

        if document.solids().is_empty() {
            explorer_note(ui, tr!(literal = "No solids yet - add one on the Setup page"));
        }
        let selection = &editor.solids_view_selection;
        let mut clicked: Option<Vec<SolidsViewRow>> = None;
        let mut clicked_blast = None;
        for kind in SolidKind::ALL {
            let solids: Vec<_> = document.solids().iter().filter(|solid| solid.kind == kind).collect();
            if solids.is_empty() {
                continue;
            }
            let kind_rows: Vec<_> = solids
                .iter()
                .flat_map(|solid| descendants(solid, editor.solid_view_bands.get(&solid.id), show_flitches))
                .collect();
            let (_, kind_heading, _) = ExplorerHeader::new(egui::Id::new(("solids_view_kind", kind as u8)), kind_label(kind))
                .icon(unthemed_icon!("layer.svg"))
                .collapse_on_click(false)
                .selected(group_selected(selection, &kind_rows))
                .show(ui, |ui| {
                    for solid in solids {
                        let occupied = editor.solid_view_bands.get(&solid.id);
                        let solid_rows = descendants(solid, occupied, show_flitches);
                        // `.1` is the heading itself; its `.inner` is the clickable
                        // label, where `.response` is only the row's hover area.
                        let (_, heading, _) = ExplorerHeader::new(egui::Id::new(("solids_view_solid", solid.id.0)), solid.name.clone())
                            .icon(unthemed_icon!("triangulation.svg"))
                            .collapse_on_click(false)
                            .selected(group_selected(selection, &solid_rows))
                            .show(ui, |ui| {
                                if occupied.is_none() {
                                    explorer_note(ui, tr!("planning-solid-geometry-pending"));
                                }
                                for bench in solid.benching.benches().iter().rev().filter(|bench| holds(occupied, bench.base, bench.top())) {
                                    let bench_band = BenchSelection {
                                        base: bench.base,
                                        top: bench.top(),
                                        is_flitch: false,
                                    };
                                    let flitches: Vec<_> = if show_flitches {
                                        bench.flitches.iter().rev().filter(|flitch| holds(occupied, flitch.base, flitch.top())).collect()
                                    } else {
                                        Vec::new()
                                    };
                                    let bench_rows: Vec<_> = std::iter::once(bench_band)
                                        .chain(flitches.iter().map(|flitch| BenchSelection {
                                            base: flitch.base,
                                            top: flitch.top(),
                                            is_flitch: true,
                                        }))
                                        .map(|band| SolidsViewRow {
                                            solid: solid.id,
                                            band: Some(band),
                                        })
                                        .collect();
                                    let bench_id = egui::Id::new(("solids_view_bench", solid.id.0, bench.base.to_bits()));
                                    let bench_label = format_rl(bench.base);
                                    let bench_selected = group_selected(selection, &bench_rows);
                                    if !show_flitches {
                                        if leaf_row(ui, bench_id, &bench_label, bench_selected) {
                                            clicked = Some(bench_rows);
                                        }
                                        continue;
                                    }
                                    // Flitches start closed: a pit carries tens of them,
                                    // and the benches are what the tree is scanned for.
                                    let bench_clicked = collapsible_row(ui, bench_id, &bench_label, bench_selected, |ui| {
                                        let blasts = solid.blasting.bench(bench.base).map(|entry| entry.blasts.as_slice()).unwrap_or_default();
                                        if blasts.is_empty() {
                                            explorer_note(ui, tr!("planning-solid-geometry-pending"));
                                            return;
                                        }
                                        for (blast_index, blast) in blasts.iter().enumerate() {
                                            let blast_ref = Some(crate::ui::state::BlastShapeRef::new(solid.id, bench.base, blast.anchor));
                                            let blast_selected = editor.selected_blast == blast_ref || (editor.selected_blast.is_none() && bench_selected);
                                            let blast_clicked = collapsible_row(ui, bench_id.with(("blast", blast_index)), &blast.name, blast_selected, |ui| {
                                                for flitch in &flitches {
                                                    let flitch_row = SolidsViewRow {
                                                        solid: solid.id,
                                                        band: Some(BenchSelection {
                                                            base: flitch.base,
                                                            top: flitch.top(),
                                                            is_flitch: true,
                                                        }),
                                                    };
                                                    if leaf_row(
                                                        ui,
                                                        egui::Id::new(("solids_view_flitch", solid.id.0, bench.base.to_bits(), blast_index, flitch.base.to_bits())),
                                                        &format_rl(flitch.base),
                                                        group_selected(selection, std::slice::from_ref(&flitch_row))
                                                            && (editor.selected_blast.is_none() || editor.selected_blast == blast_ref),
                                                    ) {
                                                        clicked = Some(vec![flitch_row]);
                                                        clicked_blast = blast_ref;
                                                    }
                                                }
                                            });
                                            if blast_clicked {
                                                clicked = Some(bench_rows.clone());
                                                clicked_blast = blast_ref;
                                            }
                                        }
                                    });
                                    if bench_clicked {
                                        clicked = Some(bench_rows);
                                    }
                                }
                            });
                        if heading.inner.clicked() {
                            clicked = Some(solid_rows);
                        }
                    }
                });
            if kind_heading.inner.clicked() {
                clicked = Some(kind_rows);
            }
        }
        paint_fixed_stripes(ui, slot, top, crate::ui::widgets::tree_row_colors(ui).1);

        if let Some(rows) = clicked {
            let extend = ui.input(|input| input.modifiers.command || input.modifiers.shift);
            if clicked_blast.is_some() && clicked_blast != editor.selected_blast && !extend {
                editor.solids_view_selection = rows;
            } else {
                apply_selection(&mut editor.solids_view_selection, &rows, extend);
            }
            editor.selected_blast = clicked_blast.filter(|_| !editor.solids_view_selection.is_empty());
            editor.selected_dig_block = None;
            ui.ctx().request_repaint();
        }
    });
}

/// Whether a band of the solid was actually generated, so the tree lists the
/// RLs the solid occupies rather than every RL the plan could name.
fn holds(occupied: Option<&Vec<BenchSelection>>, base: f64, top: f64) -> bool {
    occupied.is_some_and(|bands| bands.iter().any(|band| band.top > base + 1e-6 && band.base < top - 1e-6))
}

/// Every row a click on a solid stands for: the solid itself, and each of the
/// benches and flitches it occupies. Picking a parent picks what is under it.
fn descendants(solid: &crate::model::Solid, occupied: Option<&Vec<BenchSelection>>, include_flitches: bool) -> Vec<SolidsViewRow> {
    let mut rows = vec![SolidsViewRow { solid: solid.id, band: None }];
    for bench in solid.benching.benches() {
        let flitches = include_flitches
            .then(|| bench.flitches.iter().map(|flitch| (flitch.base, flitch.top(), true)))
            .into_iter()
            .flatten();
        for (base, top, is_flitch) in std::iter::once((bench.base, bench.top(), false)).chain(flitches) {
            if holds(occupied, base, top) {
                rows.push(SolidsViewRow {
                    solid: solid.id,
                    band: Some(BenchSelection { base, top, is_flitch }),
                });
            }
        }
    }
    rows
}

/// Whether a group reads as selected: every row of it is picked, or the bare
/// solid row that stands for the whole solid is. Picking a solid before its
/// geometry has finished still highlights the benches that then appear.
fn group_selected(selection: &[SolidsViewRow], rows: &[SolidsViewRow]) -> bool {
    !rows.is_empty()
        && rows
            .iter()
            .all(|row| selection.contains(row) || (row.band.is_some() && selection.contains(&SolidsViewRow { solid: row.solid, band: None })))
}

/// An RL with no more decimals than it needs: 348, 348.5, 348.25.
pub(crate) fn format_rl(value: f64) -> String {
    if !value.is_finite() {
        return tr!(literal = "—");
    }
    let text = format!("{value:.2}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" { "0".to_owned() } else { trimmed.to_owned() }
}

/// A selectable tree row that opens into children.
///
/// The arrow is the only thing that collapses it: clicking the row itself
/// selects it *and* its children, because the figures beside the tree are
/// read across whatever is picked.
fn collapsible_row(ui: &mut egui::Ui, id: egui::Id, label: &str, selected: bool, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let header = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
            ExplorerEntry::new(id.with("row"), label.to_owned()).selected(selected).show(ui).response
        })
        .inner;
    state.show_body_indented(&header, ui, body);
    header.clicked()
}

/// A tree row with nothing under it, gutter-aligned with the rows that have
/// an arrow so the column of labels stays straight.
fn leaf_row(ui: &mut egui::Ui, id: egui::Id, label: &str, selected: bool) -> bool {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ExplorerEntry::new(id, label.to_owned())
            .reserve_toggle_gutter(true)
            .selected(selected)
            .show(ui)
            .response
            .clicked()
    })
    .inner
}

/// Click replaces the selection with `rows`; ctrl- or shift-click toggles the
/// whole group in it. Clicking a row that is already the entire selection
/// clears it, so a parent is a toggle for everything beneath it.
fn apply_selection(selection: &mut Vec<SolidsViewRow>, rows: &[SolidsViewRow], extend: bool) {
    if !extend {
        let only = selection.len() == rows.len() && rows.iter().all(|row| selection.contains(row));
        selection.clear();
        if !only {
            selection.extend_from_slice(rows);
        }
        return;
    }
    if rows.iter().all(|row| selection.contains(row)) {
        selection.retain(|row| !rows.contains(row));
    } else {
        for row in rows {
            if !selection.contains(row) {
                selection.push(*row);
            }
        }
    }
}

/// What a property reads across the selection: one shared value, or several.
fn shared<T: PartialEq, I: IntoIterator<Item = T>>(values: I) -> Option<Option<T>> {
    let mut iter = values.into_iter();
    let first = iter.next()?;
    Some(if iter.all(|value| value == first) { Some(first) } else { None })
}

/// Render one property, saying "Multiple" when the selection disagrees.
fn read_across<T: PartialEq>(values: Vec<T>, describe: impl Fn(&T) -> String) -> String {
    match shared(values) {
        None => tr!(literal = "—"),
        Some(None) => tr!(literal = "Multiple…"),
        Some(Some(value)) => describe(&value),
    }
}

/// The properties of whatever is selected, and what it holds.
pub(crate) fn draw_properties(ui: &mut egui::Ui, rect: egui::Rect, editor: &EditorState, project: &UiProjectView, document: &Document) {
    let rows: Vec<_> = editor
        .solids_view_selection
        .iter()
        .filter_map(|row| document.solid(row.solid).map(|solid| (solid, row.band)))
        .collect();
    let title = tr!("planning-properties");
    if rows.is_empty() {
        PropertyTable::new("solids_view_properties", rect, &title).show(ui, |table| {
            table.header(&tr!("planning-property"), &tr!("planning-value"));
            table.readonly(&tr!(literal = "Selection"), &tr!(literal = "Select a solid, bench or flitch"), None, None);
        });
        return;
    }

    // The identity half of the panel is fixed height; the figures below it
    // grow with the Field List, so they get their own scrolling table.
    let identity_rows = 6 + if editor.selected_blast.is_some() { 2 } else { 0 } + if editor.selected_dig_block_info.is_some() { 6 } else { 0 };
    let split = property_table_height(ui, identity_rows).min(rect.height());
    let identity_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), split));
    let figures_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), identity_rect.bottom() + 6.0), rect.max);

    PropertyTable::new("solids_view_properties", identity_rect, &title).show(ui, |table| {
        table.header(&tr!("planning-property"), &tr!("planning-value"));
        table.readonly(
            &tr!(literal = "Type"),
            &read_across(rows.iter().map(|(solid, _)| solid.kind).collect(), |kind| kind_label(*kind)),
            None,
            None,
        );
        table.readonly(
            &tr!(literal = "Solid"),
            &read_across(rows.iter().map(|(solid, _)| solid.name.clone()).collect(), Clone::clone),
            None,
            None,
        );
        if let Some(selected) = editor.selected_blast {
            let blast = editor
                .blasting_outlines
                .iter()
                .find(|outline| crate::ui::state::BlastShapeRef::new(outline.solid, outline.bench_base, outline.anchor) == selected);
            table.readonly(&tr!(literal = "Blast"), &blast.map_or_else(|| tr!(literal = "—"), |blast| blast.name.clone()), None, None);
            table.readonly(
                &tr!(literal = "Plan area"),
                &blast.map_or_else(|| tr!(literal = "—"), |blast| format!("{:.1}", blast.area)),
                Some("m²"),
                None,
            );
        }
        if let Some(block) = &editor.selected_dig_block_info {
            // The block names its own parents. A scheduler reads the same
            // record, so the panel and the export agree by construction.
            table.readonly(&tr!(literal = "Dig block"), &block.name, None, None);
            // Lineage is shown, not inferred: a block that came out of a split
            // or a merge names what it replaced, so a schedule can be traced
            // across a redraw instead of reading as an unrelated new block.
            table.readonly(
                &tr!(literal = "Block ID"),
                &block.id.0.to_string(),
                None,
                (!block.replaces.is_empty())
                    .then(|| {
                        tr!(
                            "planning-block-replaces",
                            ids = block.replaces.iter().map(|id| id.0.to_string()).collect::<Vec<_>>().join(", ")
                        )
                    })
                    .as_deref(),
            );
            table.readonly(&tr!(literal = "Plan area"), &format!("{:.1}", block.plan_area), Some("m²"), None);
            table.readonly(&tr!(literal = "In bench"), &format!("{:.2}", block.bench.base), Some("RL"), None);
            table.readonly(&tr!(literal = "In flitch"), &format!("{:.2}", block.flitch.base), Some("RL"), None);
            table.readonly(
                &tr!(literal = "Block volume"),
                &block.volume.map_or_else(|| tr!(literal = "—"), |volume| format!("{volume:.1}")),
                Some("m³"),
                None,
            );
        }
        // A flitch names the bench it sits in, so both rows read for either.
        let benches: Vec<_> = rows.iter().map(|(solid, band)| band.map(|band| bench_base_for(solid, band)).unwrap_or(f64::NAN)).collect();
        table.readonly(&tr!(literal = "Bench RL"), &read_rl(benches), None, None);
        let flitches: Vec<_> = rows.iter().map(|(_, band)| band.filter(|band| band.is_flitch).map_or(f64::NAN, |band| band.base)).collect();
        table.readonly(&tr!(literal = "Flitch RL"), &read_rl(flitches), None, None);
        table.readonly(
            &tr!(literal = "Block Model"),
            &read_across(rows.iter().map(|(solid, _)| solid.block_model).collect(), |model| block_model_label(project, *model)),
            None,
            None,
        );
    });

    PropertyTable::new("solids_view_figures", figures_rect, &tr!(literal = "Contents")).show(ui, |table| {
        table.header(&tr!(literal = "Field"), &tr!("planning-value"));
        let volume = match &editor.solid_preview_summary {
            crate::ui::state::SolidPreviewSummary::Ready { volume: Some(volume), .. } => format!("{volume:.1}"),
            _ => tr!(literal = "—"),
        };
        table.readonly(&tr!(literal = "Volume"), &volume, Some("m³"), None);
        // Geometric volume and block-model coverage are separate figures: a
        // solid the model only partly reaches must not read as fully measured.
        if let Some(coverage) = editor.solid_view_coverage {
            table.readonly(
                &tr!("planning-reserve-coverage"),
                &format!("{:.1}", coverage * 100.0),
                Some("%"),
                Some(&tr!("planning-reserve-coverage-note")),
            );
        }
        if let Some(status) = &editor.solid_view_reserve_status {
            table.readonly(&tr!("planning-reserve-status"), status, None, None);
        }
        let reserves = editor.solid_view_reserves.as_ref();
        if let Some(reserves) = reserves {
            table.readonly(
                &tr!("planning-reserve-blocks"),
                &format!("{:.3}", reserves.all.equivalent_blocks),
                None,
                Some(&tr!("planning-reserve-method")),
            );
            table.readonly(&tr!("planning-reserve-scope"), &tr!("planning-reserve-method"), None, None);
        }
        for field in document.reserve_fields() {
            if field.aggregation == crate::model::ReserveAggregation::Category {
                let groups = reserves.and_then(|reserves| reserves.categories.get(&field.id));
                table.readonly(
                    &field.name,
                    &groups.map_or_else(|| tr!(literal = "—"), |groups| tr!("planning-reserve-categories", count = groups.len())),
                    None,
                    groups.is_none().then(|| editor.solid_view_reserve_issues.get(&field.id)).flatten().map(String::as_str),
                );
                if let Some(groups) = groups {
                    for (category, group) in groups {
                        let label = format!("{} · {}", field.name, category);
                        table.readonly(&label, &format!("{:.3}", group.equivalent_blocks), Some(&tr!("planning-reserve-blocks")), None);
                        for value_field in document
                            .reserve_fields()
                            .iter()
                            .filter(|field| field.aggregation != crate::model::ReserveAggregation::Category)
                        {
                            let value = group.numeric.get(&value_field.id).and_then(|total| total.value(value_field.aggregation));
                            table.readonly(
                                &format!("{label} · {}", value_field.name),
                                &value.map_or_else(|| tr!(literal = "—"), |value| format!("{value:.3}")),
                                None,
                                None,
                            );
                        }
                    }
                }
            } else {
                let total = reserves.and_then(|reserves| reserves.all.numeric.get(&field.id));
                let value = total.and_then(|total| total.value(field.aggregation));
                // Every dash says what is wrong with it, and a figure that
                // only some of the ground contributed to says so too. An
                // absent number is not a zero, and a partial one is not a
                // total; a reader should not have to guess which they have.
                let note = match (value, total) {
                    (None, _) => editor
                        .solid_view_reserve_issues
                        .get(&field.id)
                        .cloned()
                        .or_else(|| total.map(|total| tr!("planning-reserve-none-contributed", missing = total.missing.to_string()))),
                    (Some(_), Some(total)) if total.is_partial() => Some(tr!(
                        "planning-reserve-partial",
                        contributed = total.contributions.to_string(),
                        missing = (total.missing + total.unusable_weights).to_string()
                    )),
                    _ => None,
                };
                table.readonly(&field.name, &value.map_or_else(|| tr!(literal = "—"), |value| format!("{value:.3}")), None, note.as_deref());
            }
        }
    });
}

/// The RL column for a set of slices: blank where a row has none, and
/// "Multiple" where they disagree.
fn read_rl(values: Vec<f64>) -> String {
    if values.iter().all(|value| value.is_nan()) {
        return tr!(literal = "—");
    }
    let first = values[0];
    if values.iter().all(|value| (*value - first).abs() < 1e-6 || (value.is_nan() && first.is_nan())) {
        return format_rl(first);
    }
    tr!(literal = "Multiple…")
}

/// Which bench a slice belongs to: a bench is its own, and a flitch names the
/// bench it divides.
fn bench_base_for(solid: &crate::model::Solid, band: BenchSelection) -> f64 {
    if !band.is_flitch {
        return band.base;
    }
    solid
        .benching
        .benches()
        .into_iter()
        .find(|bench| band.base >= bench.base - 1e-6 && band.top <= bench.top() + 1e-6)
        .map_or(f64::NAN, |bench| bench.base)
}

/// The View subpage's own layout: the tree is drawn into the explorer column,
/// and this fills the rest of the window with the properties beside it.
pub(crate) fn draw_details(ui: &mut egui::Ui, editor: &mut EditorState, project: &UiProjectView, document: &Document, commands: &mut Vec<UiCommand>) -> egui::Rect {
    egui::CentralPanel::default()
        .frame(chrome::region_frame(ui))
        .show(ui, |ui| {
            let available = ui.available_rect_before_wrap();
            let area = available.shrink2(egui::vec2(0.0, 10.0_f32.min(available.height() * 0.5)));
            let properties = egui::Rect::from_min_size(area.min, egui::vec2((area.width() * 0.35).min(460.0), area.height()));
            draw_properties(ui, properties, editor, project, document);
            let inspector = egui::Rect::from_min_max(egui::pos2(properties.right() + 12.0, area.top()), area.max);
            super::planning_setup::draw_solid_render(ui, inspector, editor, commands);
            ui.allocate_rect(area, egui::Sense::hover());
        })
        .response
        .rect
}

/// Heading shown above the tree, so the column says what it is listing.
pub(crate) fn tree_heading(ui: &mut egui::Ui) {
    heading(ui, &tr!("planning-solids"));
}

/// The Blasting step's heading over the same tree stopped at benches.
pub(crate) fn bench_tree_heading(ui: &mut egui::Ui) {
    heading(ui, &tr!("planning-benches"));
}

fn heading(ui: &mut egui::Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        ui.label(bold(label));
    });
}
