//! IndexedDB persistence for encoded projects and the active session id.
//! Legacy per-asset stores are left untouched and deliberately ignored.

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, closure::Closure, prelude::*};
use wasm_bindgen_futures::JsFuture;

use crate::model::project::ProjectId;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserProjectRecord {
    pub(crate) id: ProjectId,
    pub(crate) name: String,
    pub(crate) omf_bytes: Vec<u8>,
    pub(crate) saved_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserProjectSummary {
    pub(crate) id: ProjectId,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrowserSessionProjects {
    pub(crate) projects: Vec<BrowserProjectSummary>,
    pub(crate) current_project: Option<BrowserProjectRecord>,
}

#[derive(Serialize)]
struct BrowserSessionRecord<'a> {
    key: &'static str,
    current_project_id: Option<&'a ProjectId>,
}

#[wasm_bindgen(inline_js = r#"
const DB_NAME = "incline";
const DB_VERSION = 6;
// One connection serves the whole session: opening the database costs a round
// trip through the browser's storage thread, and startup alone makes six calls.
let dbPromise = null;

function openInclineDb() {
    if (dbPromise) return dbPromise;
    dbPromise = new Promise((resolve, reject) => {
        const request = indexedDB.open(DB_NAME, DB_VERSION);
        request.onupgradeneeded = event => {
            const db = request.result;
            if (!db.objectStoreNames.contains("projects")) db.createObjectStore("projects", { keyPath: "id" });
            if (!db.objectStoreNames.contains("session")) db.createObjectStore("session", { keyPath: "key" });
            // Legacy per-asset stores are intentionally neither opened nor
            // deleted. Existing browser data remains untouched but ignored.
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
        request.onerror = () => reject(request.error || new Error("IndexedDB open failed"));
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
    const projectTx = db.transaction("projects", "readonly");
    const records = await requestValue(projectTx.objectStore("projects").getAll());
    // Old JSON-design records intentionally remain in IndexedDB but are not
    // treated as native projects after the OMF-first migration.
    const omfProjects = records.filter(record => Array.isArray(record.omf_bytes));
    const currentProject = session?.current_project_id
        ? omfProjects.find(record => record.id === session.current_project_id) || null
        : null;
    return JSON.stringify({
        projects: omfProjects.map(record => ({ id: record.id, name: record.name })),
        current_project: currentProject,
    });
}

export async function inclineGetProject(projectId) {
    const db = await openInclineDb();
    const tx = db.transaction("projects", "readonly");
    const record = await requestValue(tx.objectStore("projects").get(projectId));
    return JSON.stringify(record && Array.isArray(record.omf_bytes) ? record : null);
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
        // explanation and the Save and Exit action.
        event.returnValue = "Are you sure you want to quit with unsaved project changes?";
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
    #[wasm_bindgen(js_name = inclineGetProject)]
    fn js_get_project(project_id: &str) -> js_sys::Promise;
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

pub(crate) async fn save_session(id: Option<ProjectId>) -> Result<(), String> {
    let json = serde_json::to_string(&BrowserSessionRecord {
        key: "last",
        current_project_id: id.as_ref(),
    })
    .map_err(|error| error.to_string())?;
    JsFuture::from(js_save_session(&json)).await.map_err(js_error)?;
    Ok(())
}

async fn json_from_promise<T: serde::de::DeserializeOwned>(promise: js_sys::Promise) -> Result<T, String> {
    let value = JsFuture::from(promise).await.map_err(js_error)?;
    let json = value.as_string().ok_or_else(|| "IndexedDB returned non-text project data".to_owned())?;
    serde_json::from_str(&json).map_err(|error| format!("invalid browser project record: {error}"))
}

pub(crate) async fn load_session_projects() -> Result<BrowserSessionProjects, String> {
    json_from_promise(js_load_session_projects()).await
}

pub(crate) async fn load_project(project_id: ProjectId) -> Result<Option<BrowserProjectRecord>, String> {
    json_from_promise(js_get_project(&project_id.to_string())).await
}
