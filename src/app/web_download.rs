//! Starts browser downloads without `rfd`'s intermediate download-link dialog.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function triggerBrowserDownload(bytes, fileName, mimeType) {
    const blob = new Blob([bytes], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = fileName;
    anchor.style.display = "none";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    setTimeout(() => URL.revokeObjectURL(url), 0);
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = triggerBrowserDownload)]
    fn trigger_browser_download(bytes: &js_sys::Uint8Array, file_name: &str, mime_type: &str) -> Result<(), JsValue>;
}

pub(crate) fn download(file_name: &str, bytes: &[u8], mime_type: &str) -> Result<(), String> {
    if file_name.trim().is_empty() {
        return Err("download filename is empty".to_owned());
    }
    let bytes = js_sys::Uint8Array::from(bytes);
    trigger_browser_download(&bytes, file_name, mime_type).map_err(|error| format!("could not start browser download: {error:?}"))
}
