//! macOS system menu bar.
//!
//! Incline's other targets draw their menu inside the egui window. On macOS,
//! AppKit owns the equivalent commands so they appear in the global menu bar
//! and participate in standard Command-key handling.

use std::sync::Mutex;

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::{
    model::SceneEntityId,
    ui::state::{EditorState, UiProjectView},
};

static PENDING_ACTIONS: Mutex<Vec<MacMenuAction>> = Mutex::new(Vec::new());
static LAST_MENU_STATE: Mutex<Option<MenuState>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MenuState {
    can_save: bool,
    has_project: bool,
    can_create_terrain_tin: bool,
    can_create_block_model: bool,
    can_create_ore_triangulation: bool,
    has_design_selection: bool,
    has_selection_intersections: bool,
}

/// Actions emitted by AppKit and consumed by the winit application loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub(crate) enum MacMenuAction {
    SaveProject = 1,
    SaveProjectAs,
    CloseProject,
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
}

impl MacMenuAction {
    fn from_tag(tag: isize) -> Option<Self> {
        Some(match tag {
            value if value == Self::SaveProject as isize => Self::SaveProject,
            value if value == Self::SaveProjectAs as isize => Self::SaveProjectAs,
            value if value == Self::CloseProject as isize => Self::CloseProject,
            value if value == Self::NewProject as isize => Self::NewProject,
            value if value == Self::OpenProject as isize => Self::OpenProject,
            value if value == Self::OpenImport as isize => Self::OpenImport,
            value if value == Self::OpenExport as isize => Self::OpenExport,
            value if value == Self::ExportViewportImage as isize => Self::ExportViewportImage,
            value if value == Self::OpenPlotDialog as isize => Self::OpenPlotDialog,
            value if value == Self::RequestExit as isize => Self::RequestExit,
            value if value == Self::InsertPointsAtIntersections as isize => Self::InsertPointsAtIntersections,
            value if value == Self::OpenInsertPointAtElevation as isize => Self::OpenInsertPointAtElevation,
            value if value == Self::OpenMoveToX as isize => Self::OpenMoveToX,
            value if value == Self::OpenMoveToY as isize => Self::OpenMoveToY,
            value if value == Self::OpenMoveToZ as isize => Self::OpenMoveToZ,
            value if value == Self::OpenCreateTriangulation as isize => Self::OpenCreateTriangulation,
            value if value == Self::OpenCutTriangulationByPolyline as isize => Self::OpenCutTriangulationByPolyline,
            value if value == Self::OpenCutTriangulationByZ as isize => Self::OpenCutTriangulationByZ,
            value if value == Self::OpenCutTriangulationBySurface as isize => Self::OpenCutTriangulationBySurface,
            value if value == Self::OpenCutTopologyByPitShell as isize => Self::OpenCutTopologyByPitShell,
            value if value == Self::OpenIncludeSolidInTopology as isize => Self::OpenIncludeSolidInTopology,
            value if value == Self::OpenContourTriangulation as isize => Self::OpenContourTriangulation,
            value if value == Self::OpenPointCloudTin as isize => Self::OpenPointCloudTin,
            value if value == Self::OpenCreateBlockModel as isize => Self::OpenCreateBlockModel,
            value if value == Self::OpenCreateOreTriangulation as isize => Self::OpenCreateOreTriangulation,
            value if value == Self::OpenAbout as isize => Self::OpenAbout,
            _ => return None,
        })
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
        #[unsafe(method(performInclineMenuAction:))]
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
fn add_disabled_submenu(root: &NSMenu, title: &str, mtm: MainThreadMarker) {
    let empty_menu = menu(title, mtm);
    let item = add_submenu(root, title, &empty_menu, mtm);
    item.setEnabled(false);
}

fn add_action(menu: &NSMenu, title: &str, key: &str, action: MacMenuAction, target: &MenuTarget, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = menu_item(title, Some(sel!(performInclineMenuAction:)), key, mtm);
    item.setTag(action as isize);
    // NSMenuItem's target is weak. representedObject retains the target for
    // exactly as long as the item remains installed in the application menu.
    unsafe {
        item.setTarget(Some(target as &AnyObject));
        item.setRepresentedObject(Some(target as &AnyObject));
    }
    menu.addItem(&item);
    item
}

/// Replace winit's fallback macOS menu with Incline's native menu bar.
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
    add_action(&file_menu, "New project…", "n", MacMenuAction::NewProject, &target, mtm);
    add_action(&file_menu, "Open project…", "o", MacMenuAction::OpenProject, &target, mtm);
    add_separator(&file_menu, mtm);
    add_action(&file_menu, "Save Project", "s", MacMenuAction::SaveProject, &target, mtm);
    add_action(&file_menu, "Save Project As…", "S", MacMenuAction::SaveProjectAs, &target, mtm);
    add_action(&file_menu, "Close Project", "w", MacMenuAction::CloseProject, &target, mtm);
    add_separator(&file_menu, mtm);
    add_action(&file_menu, "Import…", "", MacMenuAction::OpenImport, &target, mtm);
    add_action(&file_menu, "Export…", "", MacMenuAction::OpenExport, &target, mtm);
    add_action(&file_menu, "Export Viewport Image…", "", MacMenuAction::ExportViewportImage, &target, mtm);
    add_action(&file_menu, "Export Engineering Drawing…", "", MacMenuAction::OpenPlotDialog, &target, mtm);
    add_submenu(&root, "File", &file_menu, mtm);

    add_disabled_submenu(&root, "Project", mtm);

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
    add_submenu(&root, "Design", &design_menu, mtm);

    let triangulation_menu = menu("Triangulation", mtm);
    triangulation_menu.setAutoenablesItems(false);
    add_action(&triangulation_menu, "Create Triangulation…", "", MacMenuAction::OpenCreateTriangulation, &target, mtm);
    add_separator(&triangulation_menu, mtm);
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

    add_disabled_submenu(&root, "Raster", mtm);

    let point_cloud_menu = menu("Point Cloud", mtm);
    point_cloud_menu.setAutoenablesItems(false);
    add_action(&point_cloud_menu, "Generate Terrain TIN…", "", MacMenuAction::OpenPointCloudTin, &target, mtm);
    add_submenu(&root, "Point Cloud", &point_cloud_menu, mtm);

    let block_model_menu = menu("Block Model", mtm);
    block_model_menu.setAutoenablesItems(false);
    add_action(&block_model_menu, "Create Block Model…", "", MacMenuAction::OpenCreateBlockModel, &target, mtm);
    add_action(&block_model_menu, "Create Ore Triangulation…", "", MacMenuAction::OpenCreateOreTriangulation, &target, mtm);
    add_submenu(&root, "Block Model", &block_model_menu, mtm);

    add_disabled_submenu(&root, "Drill Holes", mtm);

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
    if let Some(item) = find_item(root, action as isize) {
        item.setEnabled(enabled);
    }
}

/// Keep native checkmarks and availability aligned with the current editor.
pub(crate) fn sync_menu_state(editor: &EditorState, project: &UiProjectView) {
    let state = MenuState {
        can_save: project.projects.iter().any(|entry| entry.dirty),
        has_project: project.projects.iter().any(|entry| entry.is_active),
        can_create_terrain_tin: project.point_clouds.iter().any(|cloud| cloud.is_loaded),
        can_create_block_model: project.drill_holes.iter().any(|dataset| dataset.is_loaded),
        can_create_ore_triangulation: !project.block_models.is_empty(),
        has_design_selection: editor.selected_handles.iter().any(|handle| matches!(handle, SceneEntityId::Object(_))),
        has_selection_intersections: editor.selection_has_intersections,
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
    *previous = Some(state);
    drop(previous);

    set_enabled(&root, MacMenuAction::SaveProject, state.can_save);
    set_enabled(&root, MacMenuAction::SaveProjectAs, state.has_project);
    set_enabled(&root, MacMenuAction::CloseProject, state.has_project);
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
}
