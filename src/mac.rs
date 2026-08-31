//! macOS system menu bar.
//!
//! Incline Design's other targets draw their menu inside the egui window. On macOS,
//! AppKit owns the equivalent commands so they appear in the global menu bar
//! and participate in standard Command-key handling.

use std::{path::PathBuf, sync::Mutex};

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::{
    model::SceneEntityId,
    ui::state::{EditorState, UiProjectView, ViewToggle},
};

static PENDING_ACTIONS: Mutex<Vec<MacMenuAction>> = Mutex::new(Vec::new());
static LAST_MENU_STATE: Mutex<Option<MenuState>> = Mutex::new(None);

/// Not `Copy`: the Open Recent rows are part of what the menu is showing, so
/// they belong in the state the sync pass compares against.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MenuState {
    can_save: bool,
    has_project: bool,
    can_create_terrain_tin: bool,
    can_create_block_model: bool,
    can_create_ore_triangulation: bool,
    can_undrape_rasters: bool,
    has_design_selection: bool,
    has_selection_intersections: bool,
    /// Whether the active project is a file that can be shown in Finder.
    has_project_file: bool,
    /// Whether the open workspace carries the production menus.
    has_production_menus: bool,
    /// The View menu's switches, in the order [`VIEW_TOGGLES`] lists them.
    view_toggles: [bool; VIEW_TOGGLES.len()],
    /// The File > Open Recent rows, as name and the project each opens.
    recent: Vec<(String, PathBuf)>,
}

/// Actions emitted by AppKit and consumed by the winit application loop.
///
/// Every variant but [`MacMenuAction::OpenRecent`] is one fixed menu item;
/// that one names a row of the File > Open Recent submenu, which is rebuilt
/// from the recent-project list rather than written out here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MacMenuAction {
    SaveProject,
    SaveProjectAs,
    NewProject,
    OpenProject,
    OpenImport,
    OpenExport,
    ExportViewportImage,
    OpenPlotDialog,
    RequestExit,
    InsertPointsAtIntersections,
    OpenInsertPointAtElevation,
    OpenMoveToX,
    OpenMoveToY,
    OpenMoveToZ,
    OpenCreateTriangulation,
    OpenCutTriangulationByPolyline,
    OpenCutTriangulationByZ,
    OpenCutTriangulationBySurface,
    OpenCutTopologyByPitShell,
    OpenIncludeSolidInTopology,
    OpenContourTriangulation,
    OpenPointCloudTin,
    OpenCreateBlockModel,
    OpenCreateOreTriangulation,
    OpenAbout,
    UndrapeAllRasters,
    ShowProjectInFileManager,
    /// One row of File > Open Recent, by its index in the recent list the menu
    /// was last built from.
    OpenRecent(usize),
    /// One View menu switch, by its position in [`VIEW_TOGGLES`].
    ToggleView(usize),
}

/// The View menu's rows, in the order they are drawn. The egui menu bar draws
/// the same three - see [`crate::ui::elements::main_menu`].
pub(crate) const VIEW_TOGGLES: [ViewToggle; 3] = [ViewToggle::Console, ViewToggle::DarkMode, ViewToggle::XyGrid];

/// The root menus that belong to the production workspace, by title.
///
/// They are hidden with it, which is what the other targets get by leaving the
/// viewport bar's dropdowns undrawn - see
/// [`crate::ui::elements::main_menu::draw_production_menus`]. Hidden rather
/// than disabled: a workspace not carrying a menu is not the same as its
/// actions having nothing to act on, which is what greying one out says.
const PRODUCTION_MENUS: [&str; 6] = ["Design", "Triangulation", "Raster", "Point Cloud", "Block Model", "Drill Holes"];

/// Tags at or above this carry a recent-project index rather than naming a
/// fixed action, leaving room for the fixed list to grow.
const RECENT_TAG_BASE: isize = 1000;
/// Tags at or above this, and below [`RECENT_SUBMENU_TAG`], carry a View menu
/// switch by its index in [`VIEW_TOGGLES`].
const VIEW_TAG_BASE: isize = 500;
/// Tag of the Open Recent item itself. It opens a submenu rather than
/// performing anything, so [`MacMenuAction::from_tag`] rejects it: no fixed
/// action sits this far up the range.
const RECENT_SUBMENU_TAG: isize = RECENT_TAG_BASE - 1;

impl MacMenuAction {
    /// Every action that is one fixed menu item, in tag order.
    ///
    /// A variant's position here is its `NSMenuItem` tag, offset by one so
    /// that 0 - the tag an item that was never given one carries - names
    /// nothing.
    const FIXED: &'static [Self] = &[
        Self::SaveProject,
        Self::SaveProjectAs,
        Self::NewProject,
        Self::OpenProject,
        Self::OpenImport,
        Self::OpenExport,
        Self::ExportViewportImage,
        Self::OpenPlotDialog,
        Self::RequestExit,
        Self::InsertPointsAtIntersections,
        Self::OpenInsertPointAtElevation,
        Self::OpenMoveToX,
        Self::OpenMoveToY,
        Self::OpenMoveToZ,
        Self::OpenCreateTriangulation,
        Self::OpenCutTriangulationByPolyline,
        Self::OpenCutTriangulationByZ,
        Self::OpenCutTriangulationBySurface,
        Self::OpenCutTopologyByPitShell,
        Self::OpenIncludeSolidInTopology,
        Self::OpenContourTriangulation,
        Self::OpenPointCloudTin,
        Self::OpenCreateBlockModel,
        Self::OpenCreateOreTriangulation,
        Self::OpenAbout,
        Self::UndrapeAllRasters,
        Self::ShowProjectInFileManager,
    ];

    /// The `NSMenuItem` tag this action is carried by.
    fn tag(self) -> isize {
        match self {
            Self::OpenRecent(index) => RECENT_TAG_BASE + index as isize,
            Self::ToggleView(index) => VIEW_TAG_BASE + index as isize,
            action => Self::FIXED.iter().position(|fixed| *fixed == action).map_or(0, |index| index as isize + 1),
        }
    }

    fn from_tag(tag: isize) -> Option<Self> {
        if tag >= RECENT_TAG_BASE {
            return Some(Self::OpenRecent(usize::try_from(tag - RECENT_TAG_BASE).ok()?));
        }
        if tag >= VIEW_TAG_BASE && tag < RECENT_SUBMENU_TAG {
            let index = usize::try_from(tag - VIEW_TAG_BASE).ok()?;
            return (index < VIEW_TOGGLES.len()).then_some(Self::ToggleView(index));
        }
        Self::FIXED.get(usize::try_from(tag - 1).ok()?).copied()
    }
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - MenuTarget has no Drop implementation.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct MenuTarget;

    impl MenuTarget {
        // SAFETY: This signature matches the AppKit target/action selector.
        #[unsafe(method(performInclineDesignMenuAction:))]
        fn perform_action(&self, sender: &NSMenuItem) {
            if let Some(action) = MacMenuAction::from_tag(sender.tag()) {
                action_queue().push(action);
            }
        }
    }

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` has the declared signature.
        unsafe { msg_send![super(this), init] }
    }
}

fn action_queue() -> std::sync::MutexGuard<'static, Vec<MacMenuAction>> {
    PENDING_ACTIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn last_menu_state() -> std::sync::MutexGuard<'static, Option<MenuState>> {
    LAST_MENU_STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Take every menu action queued since the last application-loop iteration.
pub(crate) fn drain_actions() -> Vec<MacMenuAction> {
    std::mem::take(&mut *action_queue())
}

fn menu(title: &str, mtm: MainThreadMarker) -> Retained<NSMenu> {
    NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title))
}

fn menu_item(title: &str, selector: Option<Sel>, key: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    // SAFETY: Each selector supplied by this module is implemented either by
    // MenuTarget or by AppKit's standard responder chain.
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), selector, &NSString::from_str(key)) }
}

fn add_separator(menu: &NSMenu, mtm: MainThreadMarker) {
    menu.addItem(&NSMenuItem::separatorItem(mtm));
}

fn add_submenu(root: &NSMenu, title: &str, submenu: &NSMenu, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = menu_item(title, None, "", mtm);
    item.setSubmenu(Some(submenu));
    root.addItem(&item);
    item
}

/// Add a placeholder top-level menu with no actions wired up yet.
///
/// Unused while every menu has content - kept as the counterpart to
/// `MenuBarMenu::enabled` on the egui side.
#[allow(dead_code)]
fn add_disabled_submenu(root: &NSMenu, title: &str, mtm: MainThreadMarker) {
    let empty_menu = menu(title, mtm);
    let item = add_submenu(root, title, &empty_menu, mtm);
    item.setEnabled(false);
}

fn add_action(menu: &NSMenu, title: &str, key: &str, action: MacMenuAction, target: &MenuTarget, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = menu_item(title, Some(sel!(performInclineDesignMenuAction:)), key, mtm);
    item.setTag(action.tag());
    // NSMenuItem's target is weak. representedObject retains the target for
    // exactly as long as the item remains installed in the application menu.
    unsafe {
        item.setTarget(Some(target as &AnyObject));
        item.setRepresentedObject(Some(target as &AnyObject));
    }
    menu.addItem(&item);
    item
}

/// Replace winit's fallback macOS menu with Incline Design's native menu bar.
pub(crate) fn install_menu_bar() {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("Cannot install the macOS menu bar away from the main thread");
        return;
    };

    let app = NSApplication::sharedApplication(mtm);
    let target = MenuTarget::new(mtm);
    let root = menu(crate::APP_NAME, mtm);
    *last_menu_state() = None;

    let application_menu = menu(crate::APP_NAME, mtm);
    application_menu.setAutoenablesItems(false);
    add_action(&application_menu, &format!("About {}", crate::APP_NAME), "", MacMenuAction::OpenAbout, &target, mtm);
    add_separator(&application_menu, mtm);
    add_action(&application_menu, &format!("Quit {}", crate::APP_NAME), "q", MacMenuAction::RequestExit, &target, mtm);
    add_submenu(&root, crate::APP_NAME, &application_menu, mtm);

    let file_menu = menu("File", mtm);
    file_menu.setAutoenablesItems(false);
    add_action(&file_menu, "New Project…", "n", MacMenuAction::NewProject, &target, mtm);
    add_action(&file_menu, "Open Project…", "o", MacMenuAction::OpenProject, &target, mtm);
    // Empty until the first sync pass fills it - see `rebuild_recent_menu`.
    let recent_menu = menu("Open Recent", mtm);
    recent_menu.setAutoenablesItems(false);
    let recent_item = add_submenu(&file_menu, "Open Recent", &recent_menu, mtm);
    recent_item.setTag(RECENT_SUBMENU_TAG);
    recent_item.setEnabled(false);
    add_action(&file_menu, "Reveal in Finder", "", MacMenuAction::ShowProjectInFileManager, &target, mtm);
    add_separator(&file_menu, mtm);
    add_action(&file_menu, "Save Project", "s", MacMenuAction::SaveProject, &target, mtm);
    add_action(&file_menu, "Save Project As…", "S", MacMenuAction::SaveProjectAs, &target, mtm);
    add_separator(&file_menu, mtm);
    add_action(&file_menu, "Import…", "", MacMenuAction::OpenImport, &target, mtm);
    add_action(&file_menu, "Export…", "", MacMenuAction::OpenExport, &target, mtm);
    add_action(&file_menu, "Export Viewport Image…", "", MacMenuAction::ExportViewportImage, &target, mtm);
    add_action(&file_menu, "Export Engineering Drawing…", "", MacMenuAction::OpenPlotDialog, &target, mtm);
    add_submenu(&root, "File", &file_menu, mtm);

    // Switches onto the same settings the Interface preferences tab holds;
    // their checkmarks are kept current by `sync_menu_state`.
    let view_menu = menu("View", mtm);
    view_menu.setAutoenablesItems(false);
    for (index, toggle) in VIEW_TOGGLES.iter().enumerate() {
        add_action(&view_menu, toggle.label(), "", MacMenuAction::ToggleView(index), &target, mtm);
    }
    add_submenu(&root, "View", &view_menu, mtm);

    let design_menu = menu("Design", mtm);
    design_menu.setAutoenablesItems(false);
    let insert_point_menu = menu("Insert Point", mtm);
    insert_point_menu.setAutoenablesItems(false);
    add_action(&insert_point_menu, "At Intersection", "", MacMenuAction::InsertPointsAtIntersections, &target, mtm);
    add_action(&insert_point_menu, "At Elevation…", "", MacMenuAction::OpenInsertPointAtElevation, &target, mtm);
    add_submenu(&design_menu, "Insert Point", &insert_point_menu, mtm);
    add_separator(&design_menu, mtm);
    let move_to_menu = menu("Move to", mtm);
    move_to_menu.setAutoenablesItems(false);
    add_action(&move_to_menu, "Set X…", "", MacMenuAction::OpenMoveToX, &target, mtm);
    add_action(&move_to_menu, "Set Y…", "", MacMenuAction::OpenMoveToY, &target, mtm);
    add_action(&move_to_menu, "Set Z…", "", MacMenuAction::OpenMoveToZ, &target, mtm);
    add_submenu(&design_menu, "Move to", &move_to_menu, mtm);
    add_separator(&design_menu, mtm);
    add_action(&design_menu, "Create Triangulation…", "", MacMenuAction::OpenCreateTriangulation, &target, mtm);
    add_submenu(&root, "Design", &design_menu, mtm);

    let triangulation_menu = menu("Triangulation", mtm);
    triangulation_menu.setAutoenablesItems(false);
    add_action(
        &triangulation_menu,
        "Clip Surface by Polyline…",
        "",
        MacMenuAction::OpenCutTriangulationByPolyline,
        &target,
        mtm,
    );
    add_action(
        &triangulation_menu,
        "Slice Triangulation by Z Range…",
        "",
        MacMenuAction::OpenCutTriangulationByZ,
        &target,
        mtm,
    );
    add_action(&triangulation_menu, "Trim to Topology…", "", MacMenuAction::OpenCutTriangulationBySurface, &target, mtm);
    add_separator(&triangulation_menu, mtm);
    add_action(
        &triangulation_menu,
        "Cut Topology with Pit Shell…",
        "",
        MacMenuAction::OpenCutTopologyByPitShell,
        &target,
        mtm,
    );
    add_action(
        &triangulation_menu,
        "Merge Shell into Topology…",
        "",
        MacMenuAction::OpenIncludeSolidInTopology,
        &target,
        mtm,
    );
    add_separator(&triangulation_menu, mtm);
    add_action(&triangulation_menu, "Generate Contour Lines…", "", MacMenuAction::OpenContourTriangulation, &target, mtm);
    add_submenu(&root, "Triangulation", &triangulation_menu, mtm);

    let raster_menu = menu("Raster", mtm);
    raster_menu.setAutoenablesItems(false);
    add_action(&raster_menu, "Undrape All", "", MacMenuAction::UndrapeAllRasters, &target, mtm);
    add_submenu(&root, "Raster", &raster_menu, mtm);

    let point_cloud_menu = menu("Point Cloud", mtm);
    point_cloud_menu.setAutoenablesItems(false);
    add_action(&point_cloud_menu, "Create Triangulation…", "", MacMenuAction::OpenPointCloudTin, &target, mtm);
    add_submenu(&root, "Point Cloud", &point_cloud_menu, mtm);

    let block_model_menu = menu("Block Model", mtm);
    block_model_menu.setAutoenablesItems(false);
    add_action(&block_model_menu, "Create Ore Triangulation…", "", MacMenuAction::OpenCreateOreTriangulation, &target, mtm);
    add_submenu(&root, "Block Model", &block_model_menu, mtm);

    let drill_hole_menu = menu("Drill Holes", mtm);
    drill_hole_menu.setAutoenablesItems(false);
    add_action(&drill_hole_menu, "Create Block Model…", "", MacMenuAction::OpenCreateBlockModel, &target, mtm);
    add_submenu(&root, "Drill Holes", &drill_hole_menu, mtm);

    app.setMainMenu(Some(&root));
    NSMenu::setMenuBarVisible(true, mtm);
}

fn find_item(menu: &NSMenu, tag: isize) -> Option<Retained<NSMenuItem>> {
    for item in menu.itemArray().iter() {
        if item.tag() == tag {
            return Some(item);
        }
        if let Some(submenu) = item.submenu()
            && let Some(found) = find_item(&submenu, tag)
        {
            return Some(found);
        }
    }
    None
}

fn set_enabled(root: &NSMenu, action: MacMenuAction, enabled: bool) {
    if let Some(item) = find_item(root, action.tag()) {
        item.setEnabled(enabled);
    }
}

/// Show or hide the production workspace's root menus.
fn set_production_menus_visible(root: &NSMenu, visible: bool) {
    for title in PRODUCTION_MENUS {
        if let Some(item) = root.itemWithTitle(&NSString::from_str(title)) {
            item.setHidden(!visible);
        }
    }
}

fn set_checked(root: &NSMenu, action: MacMenuAction, checked: bool) {
    if let Some(item) = find_item(root, action.tag()) {
        item.setState(if checked { NSControlStateValueOn } else { NSControlStateValueOff });
    }
}

/// Refill File > Open Recent from the recent-project list.
///
/// The rows are rebuilt rather than enabled and disabled in place: what the
/// list holds changes with every project opened, and a row's tag is its index
/// in it. Each new item retains its own target, exactly as the fixed ones
/// installed at startup do.
fn rebuild_recent_menu(root: &NSMenu, recent: &[(String, PathBuf)], mtm: MainThreadMarker) {
    let Some(item) = find_item(root, RECENT_SUBMENU_TAG) else {
        return;
    };
    let Some(submenu) = item.submenu() else {
        return;
    };
    submenu.removeAllItems();
    let target = MenuTarget::new(mtm);
    for (index, (name, _)) in recent.iter().enumerate() {
        add_action(&submenu, name, "", MacMenuAction::OpenRecent(index), &target, mtm);
    }
    // Greyed out with nothing to offer rather than hidden, so the File menu
    // keeps its shape.
    item.setEnabled(!recent.is_empty());
}

/// The project behind one File > Open Recent row, as the menu last showed it.
///
/// The click carries the row's index alone, so it is resolved against the
/// same list the row was built from rather than against the current one.
pub(crate) fn recent_project_path(index: usize) -> Option<PathBuf> {
    let state = last_menu_state();
    state.as_ref()?.recent.get(index).map(|(_, path)| path.clone())
}

/// Keep native checkmarks and availability aligned with the current editor.
pub(crate) fn sync_menu_state(editor: &EditorState, project: &UiProjectView) {
    let state = MenuState {
        can_save: project.projects.iter().any(crate::ui::state::UiProjectEntry::needs_save),
        has_project: project.projects.iter().any(|entry| entry.is_active),
        can_create_terrain_tin: project.point_clouds.iter().any(|cloud| cloud.is_loaded),
        can_create_block_model: project.drill_holes.iter().any(|dataset| dataset.is_loaded),
        can_create_ore_triangulation: !project.block_models.is_empty(),
        can_undrape_rasters: project.raster_textures.iter().any(|raster| raster.is_draped),
        has_design_selection: editor.selected_handles.iter().any(|handle| matches!(handle, SceneEntityId::Object(_))),
        has_selection_intersections: editor.selection_has_intersections,
        has_project_file: project.active_path.is_some(),
        has_production_menus: editor.active_workspace.has_production_tools(),
        view_toggles: {
            let preferences = editor.current_preferences();
            VIEW_TOGGLES.map(|toggle| toggle.get(&preferences))
        },
        recent: project.recent_projects().map(|entry| (entry.name.clone(), entry.path.clone())).collect(),
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(root) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return;
    };
    let mut previous = last_menu_state();
    if previous.as_ref() == Some(&state) {
        return;
    }
    *previous = Some(state.clone());
    drop(previous);

    rebuild_recent_menu(&root, &state.recent, mtm);
    set_production_menus_visible(&root, state.has_production_menus);

    set_enabled(&root, MacMenuAction::SaveProject, state.can_save);
    set_enabled(&root, MacMenuAction::SaveProjectAs, state.has_project);
    // A never-saved project is nowhere to be shown.
    set_enabled(&root, MacMenuAction::ShowProjectInFileManager, state.has_project_file);
    set_enabled(&root, MacMenuAction::UndrapeAllRasters, state.can_undrape_rasters);
    set_enabled(&root, MacMenuAction::OpenPointCloudTin, state.can_create_terrain_tin);
    set_enabled(&root, MacMenuAction::OpenCreateBlockModel, state.can_create_block_model);
    set_enabled(&root, MacMenuAction::OpenCreateOreTriangulation, state.can_create_ore_triangulation);
    // The Design menu only acts on selected design objects.
    for action in [
        MacMenuAction::OpenInsertPointAtElevation,
        MacMenuAction::OpenMoveToX,
        MacMenuAction::OpenMoveToY,
        MacMenuAction::OpenMoveToZ,
    ] {
        set_enabled(&root, action, state.has_design_selection);
    }
    // Inserting at intersections additionally needs two polylines that cross.
    set_enabled(&root, MacMenuAction::InsertPointsAtIntersections, state.has_selection_intersections);
    for (index, checked) in state.view_toggles.iter().enumerate() {
        set_checked(&root, MacMenuAction::ToggleView(index), *checked);
    }
}
