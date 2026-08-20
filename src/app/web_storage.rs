//! IndexedDB persistence for browser PIDBs, imported and generated data, and
//! the last open-project set.

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, closure::Closure, prelude::*};
use wasm_bindgen_futures::JsFuture;

use crate::model::pidb::{PidbFile, ProjectId};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserProjectRecord {
    pub(crate) id: ProjectId,
    pub(crate) name: String,
    pub(crate) pidb: PidbFile,
    pub(crate) saved_at_ms: u64,
}

/// The byte-backed data an explorer entry can be restored from. Each variant
/// is its own IndexedDB object store, so a listing walks one kind of data
/// without touching the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BrowserStore {
    /// Generated and imported meshes, as PLY bytes.
    Triangulations,
    /// Imported point clouds, as their original file bytes.
    PointClouds,
    /// Imported CSV block models, as their original bytes.
    BlockModels,
    /// Imported rasters, as their original file bytes.
    Rasters,
    /// Drillhole manifests containing a mapped CSV bundle.
    DrillHoles,
}

impl BrowserStore {
    fn store_name(self) -> &'static str {
        match self {
            Self::Triangulations => "triangulations",
            Self::PointClouds => "point_clouds",
            Self::BlockModels => "block_models",
            Self::Rasters => "rasters",
            Self::DrillHoles => "drill_holes",
        }
    }

    /// What the data is called in messages about a failed read or write.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Triangulations => "triangulation",
            Self::PointClouds => "point cloud",
            Self::BlockModels => "block model",
            Self::Rasters => "raster",
            Self::DrillHoles => "drillhole dataset",
        }
    }
}

/// One piece of data held in browser storage, as the bytes of the file format
/// it would occupy on disk. The browser has no filesystem to hold an import or
/// a generated mesh in, so browser storage takes the role a file on disk plays
/// for the desktop build: unloading frees the memory and leaves the record
/// behind to load again.
#[derive(Clone, Debug)]
pub(crate) struct BrowserAssetRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) metadata_json: Option<String>,
}

/// A stored record as the explorer lists it, before its bytes are read.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BrowserAssetSummary {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Serialize)]
struct BrowserSessionRecord<'a> {
    key: &'static str,
    open_project_ids: &'a [ProjectId],
}

#[wasm_bindgen(inline_js = r#"
const DB_NAME = "incline";
const DB_VERSION = 5;
// Every byte-backed store shares one shape ({ id, name, bytes, saved_at_ms }),
// so they are created and accessed through the same helpers below.
const ASSET_STORES = ["triangulations", "point_clouds", "block_models", "rasters", "drill_holes"];

// One connection serves the whole session: opening the database costs a round
// trip through the browser's storage thread, and startup alone makes six calls.
let dbPromise = null;

function openInclineDb() {
    if (dbPromise) return dbPromise;
    dbPromise = new Promise((resolve, reject) => {
        const request = indexedDB.open(DB_NAME, DB_VERSION);
        let unsupportedVersion = null;
        request.onupgradeneeded = event => {
            if (event.oldVersion !== 0) {
                unsupportedVersion = event.oldVersion;
                request.transaction.abort();
                return;
            }
            const db = request.result;
            db.createObjectStore("projects", { keyPath: "id" });
            db.createObjectStore("session", { keyPath: "key" });
            for (const store of ASSET_STORES) {
                const objectStore = db.createObjectStore(store, { keyPath: "id" });
                // Listing walks this index rather than the records themselves,
                // which is what keeps the stored bytes out of a listing.
                objectStore.createIndex("name", "name", { unique: false });
            }
        };
        request.onsuccess = () => {
            const db = request.result;
            // Another tab upgrading the schema is blocked until this connection
            // closes, so give up the cached one as soon as that is asked for.
            db.onversionchange = () => {
                dbPromise = null;
                db.close();
            };
            db.onclose = () => {
                dbPromise = null;
            };
            resolve(db);
        };
        request.onerror = () => reject(unsupportedVersion === null
            ? request.error || new Error("IndexedDB open failed")
            : new Error(`IndexedDB schema version ${unsupportedVersion} is unsupported (expected ${DB_VERSION})`));
        request.onblocked = () => reject(new Error("IndexedDB upgrade is blocked by another Incline tab"));
    });
    // A failed open must not be cached, or every later call replays the failure.
    dbPromise = dbPromise.catch(error => {
        dbPromise = null;
        throw error;
    });
    return dbPromise;
}

function requestValue(request) {
    return new Promise((resolve, reject) => {
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error || new Error("IndexedDB request failed"));
    });
}

function transactionDone(transaction) {
    return new Promise((resolve, reject) => {
        transaction.oncomplete = () => resolve();
        transaction.onerror = () => reject(transaction.error || new Error("IndexedDB transaction failed"));
        transaction.onabort = () => reject(transaction.error || new Error("IndexedDB transaction aborted"));
    });
}

export async function inclinePutProject(recordJson) {
    const db = await openInclineDb();
    const tx = db.transaction("projects", "readwrite");
    tx.objectStore("projects").put(JSON.parse(recordJson));
    await transactionDone(tx);
}

export async function inclineDeleteProject(projectId) {
    const db = await openInclineDb();
    const tx = db.transaction("projects", "readwrite");
    tx.objectStore("projects").delete(projectId);
    await transactionDone(tx);
}

export async function inclineSaveSession(sessionJson) {
    const db = await openInclineDb();
    const tx = db.transaction("session", "readwrite");
    tx.objectStore("session").put(JSON.parse(sessionJson));
    await transactionDone(tx);
}

export async function inclineLoadSessionProjects() {
    const db = await openInclineDb();
    const sessionTx = db.transaction("session", "readonly");
    const session = await requestValue(sessionTx.objectStore("session").get("last"));
    if (!session || !Array.isArray(session.open_project_ids)) return "[]";
    const projectTx = db.transaction("projects", "readonly");
    const store = projectTx.objectStore("projects");
    const records = await Promise.all(session.open_project_ids.map(id => requestValue(store.get(id))));
    return JSON.stringify(records.filter(Boolean));
}

export async function inclinePutAsset(store, id, name, bytes, metadataJson) {
    const db = await openInclineDb();
    const tx = db.transaction(store, "readwrite");
    // `bytes` is already a copy owned by the JS heap (js_sys copies out of
    // the shared wasm memory), so it structured-clones into IndexedDB as is.
    tx.objectStore(store).put({ id, name, bytes, metadata_json: metadataJson || null, saved_at_ms: Date.now() });
    await transactionDone(tx);
}

export async function inclineDeleteAsset(store, id) {
    const db = await openInclineDb();
    const tx = db.transaction(store, "readwrite");
    tx.objectStore(store).delete(id);
    await transactionDone(tx);
}

// Names only, taken from the `name` index with a key cursor: a cursor over the
// records themselves would deserialize every stored file's bytes just to label
// the explorer. Here `cursor.key` is the name and `cursor.primaryKey` the id,
// so no record is ever read. Entries come back in name order.
export async function inclineListAssets(store) {
    const db = await openInclineDb();
    const tx = db.transaction(store, "readonly");
    const request = tx.objectStore(store).index("name").openKeyCursor();
    const summaries = await new Promise((resolve, reject) => {
        const found = [];
        request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor) return resolve(found);
            found.push({ id: cursor.primaryKey, name: cursor.key });
            cursor.continue();
        };
        request.onerror = () => reject(request.error || new Error("IndexedDB cursor failed"));
    });
    return JSON.stringify(summaries);
}

export async function inclineGetAsset(store, id) {
    const db = await openInclineDb();
    const tx = db.transaction(store, "readonly");
    return await requestValue(tx.objectStore(store).get(id));
}

export async function inclineListProjects() {
    const db = await openInclineDb();
    const tx = db.transaction("projects", "readonly");
    return JSON.stringify(await requestValue(tx.objectStore("projects").getAll()));
}

export function inclineInstallDirtyGuard() {
    if (window.__inclineDirtyGuardInstalled) return;
    window.__inclineDirtyGuardInstalled = true;
    window.__inclineDirty = false;
    addEventListener("beforeunload", event => {
        if (!window.__inclineDirty) return;
        event.preventDefault();
        // Modern browsers deliberately replace this with their own standard
        // confirmation text. The in-app Exit action provides Incline's full
        // explanation and the Download All PIDBs button.
        event.returnValue = "Are you sure you want to quit? Export your PIDBs before leaving.";
    });
}

export function inclineSetDirty(dirty) {
    window.__inclineDirty = Boolean(dirty);
}

export function inclineInstallPasteListener(callback) {
    if (window.__inclinePasteListenerInstalled) return;
    window.__inclinePasteListenerInstalled = true;

    const isEditable = target =>
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target?.isContentEditable;
    const releasePasteShortcut = event => {
        if (event.repeat || isEditable(event.target)) return;
        if (!(event.ctrlKey || event.metaKey) || event.altKey || event.key.toLowerCase() !== "v") {
            return;
        }
        // Winit normally prevents the canvas key event's default action. Stop
        // it reaching that listener without cancelling the browser default,
        // which produces the permission-free paste event below.
        event.stopImmediatePropagation();
    };
    addEventListener("keydown", releasePasteShortcut, true);
    addEventListener("keyup", releasePasteShortcut, true);

    addEventListener("paste", event => {
        if (isEditable(event.target)) return;
        const text = event.clipboardData?.getData("text/plain");
        if (!text) return;
        event.preventDefault();
        callback(text);
    });
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = inclinePutProject)]
    fn js_put_project(record_json: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineDeleteProject)]
    fn js_delete_project(project_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineSaveSession)]
    fn js_save_session(session_json: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineLoadSessionProjects)]
    fn js_load_session_projects() -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclinePutAsset)]
    fn js_put_asset(store: &str, id: &str, name: &str, bytes: &js_sys::Uint8Array, metadata_json: Option<&str>) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineDeleteAsset)]
    fn js_delete_asset(store: &str, id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineListAssets)]
    fn js_list_assets(store: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineGetAsset)]
    fn js_get_asset(store: &str, id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inclineInstallDirtyGuard)]
    fn js_install_dirty_guard();
    #[wasm_bindgen(js_name = inclineSetDirty)]
    fn js_set_dirty(dirty: bool);
    #[wasm_bindgen(js_name = inclineInstallPasteListener)]
    fn js_install_paste_listener(callback: &js_sys::Function);
}

pub(crate) fn install_dirty_guard() {
    js_install_dirty_guard();
}

pub(crate) fn set_dirty(dirty: bool) {
    js_set_dirty(dirty);
}

pub(crate) fn install_paste_listener(proxy: winit::event_loop::EventLoopProxy<crate::app::AppEvent>) {
    let callback = Closure::wrap(Box::new(move |text: String| {
        let _ = proxy.send_event(crate::app::AppEvent::BrowserClipboardPasted(text));
    }) as Box<dyn FnMut(String)>);
    js_install_paste_listener(callback.as_ref().unchecked_ref());
    // The JavaScript listener lives for the page lifetime and is installed at
    // most once, so the matching Rust closure intentionally does too.
    callback.forget();
}

fn js_error(error: JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("browser storage error: {error:?}"))
}

pub(crate) async fn put_project(record: &BrowserProjectRecord) -> Result<(), String> {
    let json = serde_json::to_string(record).map_err(|error| error.to_string())?;
    JsFuture::from(js_put_project(&json)).await.map_err(js_error)?;
    Ok(())
}

pub(crate) async fn delete_project(project_id: ProjectId) -> Result<(), String> {
    JsFuture::from(js_delete_project(&project_id.to_string())).await.map_err(js_error)?;
    Ok(())
}

pub(crate) async fn save_session(ids: &[ProjectId]) -> Result<(), String> {
    let json = serde_json::to_string(&BrowserSessionRecord {
        key: "last",
        open_project_ids: ids,
    })
    .map_err(|error| error.to_string())?;
    JsFuture::from(js_save_session(&json)).await.map_err(js_error)?;
    Ok(())
}

async fn records_from_promise(promise: js_sys::Promise) -> Result<Vec<BrowserProjectRecord>, String> {
    let value = JsFuture::from(promise).await.map_err(js_error)?;
    let json = value.as_string().ok_or_else(|| "IndexedDB returned a non-text project list".to_owned())?;
    serde_json::from_str(&json).map_err(|error| format!("invalid browser project record: {error}"))
}

pub(crate) async fn load_session_projects() -> Result<Vec<BrowserProjectRecord>, String> {
    records_from_promise(js_load_session_projects()).await
}

pub(crate) async fn put_asset(store: BrowserStore, id: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
    put_staged_asset(store, id.to_owned(), name.to_owned(), stage_bytes(bytes), None).await
}

/// Copy `bytes` into a JS-owned buffer. IndexedDB cannot structured-clone a
/// view onto the shared wasm memory, so the copy is required rather than
/// incidental — but taking it up front lets a caller that only borrows the
/// bytes hand the rest of the write to `spawn_local` without cloning them into
/// a second Rust allocation as well.
pub(crate) fn stage_bytes(bytes: &[u8]) -> js_sys::Uint8Array {
    js_sys::Uint8Array::from(bytes)
}

pub(crate) async fn put_staged_asset(store: BrowserStore, id: String, name: String, bytes: js_sys::Uint8Array, metadata_json: Option<String>) -> Result<(), String> {
    JsFuture::from(js_put_asset(store.store_name(), &id, &name, &bytes, metadata_json.as_deref()))
        .await
        .map_err(js_error)?;
    Ok(())
}

pub(crate) async fn delete_asset(store: BrowserStore, id: &str) -> Result<(), String> {
    JsFuture::from(js_delete_asset(store.store_name(), id)).await.map_err(js_error)?;
    Ok(())
}

/// Every stored record's key and name in `store`, without its bytes.
pub(crate) async fn list_assets(store: BrowserStore) -> Result<Vec<BrowserAssetSummary>, String> {
    let value = JsFuture::from(js_list_assets(store.store_name())).await.map_err(js_error)?;
    let label = store.label();
    let json = value.as_string().ok_or_else(|| format!("IndexedDB returned a non-text {label} list"))?;
    serde_json::from_str(&json).map_err(|error| format!("invalid stored {label} list: {error}"))
}

/// Read one stored record, bytes included. `None` when the record is gone
/// (deleted in another tab, or cleared by the browser).
pub(crate) async fn get_asset(store: BrowserStore, id: &str) -> Result<Option<BrowserAssetRecord>, String> {
    let entry = JsFuture::from(js_get_asset(store.store_name(), id)).await.map_err(js_error)?;
    if entry.is_undefined() || entry.is_null() {
        return Ok(None);
    }
    let label = store.label();
    let field = |name: &str| js_sys::Reflect::get(&entry, &JsValue::from_str(name)).map_err(js_error);
    let id = field("id")?.as_string().ok_or_else(|| format!("stored {label} has no id"))?;
    let name = field("name")?.as_string().unwrap_or_else(|| label.to_owned());
    let metadata_json = field("metadata_json")?.as_string();
    let bytes = field("bytes")?;
    let bytes = bytes
        .dyn_ref::<js_sys::Uint8Array>()
        .ok_or_else(|| format!("stored {label} '{name}' has no data"))?
        .to_vec();
    Ok(Some(BrowserAssetRecord { id, name, bytes, metadata_json }))
}
