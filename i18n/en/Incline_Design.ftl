# Incline — English message catalog (canonical source).
#
# Every `tr!(...)` call in the code is checked against THIS file at compile time:
# an unknown id or a missing argument fails the build. Other languages
# (`i18n/<lang>/Incline.ftl`) may be incomplete and fall back here.
#
# Ids are kebab-case, grouped by area with a prefix (`menu-`, `settings-`,
# `tri-`, `common-`, ...). Keep this file grouped and roughly sorted.

## Shared

common-cancel = Cancel
common-clear = Clear
common-close = Close
common-color = Color
common-fill = Fill

## Preferences — Interface

settings-language = Language
settings-language-system = System
settings-language-restart-hint = Language changes take effect the next time Incline starts.

## Menu bar — File

menu-file = File
menu-file-save-project = Save Project
menu-file-save-project-as = Save Project As...
menu-file-new-project = New Project...
menu-file-open-project = Open Project...
menu-file-open-recent = Open Recent
menu-file-show-in-explorer = Show in Explorer
menu-file-show-in-folder = Open Containing Folder
menu-file-import = Import...
menu-file-export = Export...
menu-file-export-viewport-image = Export Viewport Image...
menu-file-export-engineering-drawing = Export Engineering Drawing...
menu-file-about = About { $app }...
menu-file-exit = Exit Application

## Menu bar — View

menu-view = View

## Workspaces

ws-production = Production
ws-drill-and-blast = Drill & Blast
ws-geology = Geology

## Menubars


ws-menubar-design = Design
ws-menubar-triangulation = Triangulation
ws-menubar-raster = Raster
ws-menubar-point-cloud = Point Cloud
ws-menubar-block-model = Block Model
ws-menubar-drillholes = Drillholes
ws-menubar-active-layer = Layer:

## Rename / delete item dialogs

# { $kind } is a workspace noun from the ws-production-* set above.
dialog-rename-title = Rename { $kind }
dialog-rename-field = New name
dialog-rename-field-hint = Required
dialog-rename-submit = Rename
dialog-delete-title = Delete { $kind }
dialog-delete-confirm =
    Delete '{ $name }' from the project?
    This cannot be undone.


## Create Triangulation dialog

tri-create-title = Create Triangulation
tri-create-help = Click objects in the viewport to select/deselect. Drag to box-select.
tri-create-type-label = Triangulation type
tri-create-type-help =
    Open surface creates a terrain-style sheet. Solid creates a fully enclosed
    mesh and requires input that can form a watertight boundary.
tri-create-output-name = Output name
tri-create-output-name-help = Name assigned to the generated triangulation.
tri-create-output-name-hint = triangulation name
tri-create-run = Triangulate

tri-selection-none = No objects selected yet.
tri-selection-selected = { $summary } selected

tri-type-open-surface = Open surface
tri-type-solid-closed = Solid – fully closed

# Selection summary pieces, e.g. "3 polylines, 1 point". Each noun is pluralised
# by its own count so languages with more than two plural forms read correctly.
tri-count-polylines =
    { $count ->
        [one] { $count } polyline
       *[other] { $count } polylines
    }
tri-count-strings =
    { $count ->
        [one] { $count } string
       *[other] { $count } strings
    }
tri-count-points =
    { $count ->
        [one] { $count } point
       *[other] { $count } points
    }
tri-count-texts =
    { $count ->
        [one] { $count } text object
       *[other] { $count } text objects
    }
tri-count-objects =
    { $count ->
        [one] { $count } object
       *[other] { $count } objects
    }

about-read-full-licence = Read the full licence ↗
about-source-code = Source Code
about-website = Website