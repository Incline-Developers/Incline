// Disable console window on Windows in release builds
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod fonts;
mod i18n;
mod logging;
#[cfg(target_os = "macos")]
mod mac;
mod model;
mod natural_sort;
mod rendering;
mod ui;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Result;
use winit::event_loop::EventLoop;

use crate::app::App;

pub(crate) type Size = (f32, f32);
/// Application display name.
///
/// A literal rather than `CARGO_PKG_NAME`: the crate name cannot hold the
/// space the product name has.
pub(crate) const APP_NAME: &str = "Incline Design";
/// Native package identifier used by platform integrations.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const APP_ID: &str = "net.inclinedesign.incline";
/// Application release version.
pub(crate) const APP_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// Desktop main function
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    let _logging_guard = logging::init();
    (|| {
        let event_loop = EventLoop::new()?;
        let mut app = App::new()?;
        // App construction loads the saved language. Log startup afterwards so
        // the terminal and activity console use that language too.
        logging::startup_log();
        event_loop.run_app(&mut app)?;
        Ok(())
    })()
}

/// Web main function
#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    logging::init();

    wasm_bindgen_futures::spawn_local(async {
        if let Err(error) = start_web().await {
            log::error!("{}", crate::i18n::tr_format!(literal = "Incline Design Web startup failed: %error%", error = error));
            show_web_startup_error(&error);
        }
    });
}

#[cfg(target_arch = "wasm32")]
async fn start_web() -> std::result::Result<(), String> {
    use winit::platform::web::EventLoopExtWebSys;

    use crate::app::AppEvent;

    let threads = web_sys::window().map(|window| window.navigator().hardware_concurrency() as usize).unwrap_or(1).clamp(1, 4);
    wasm_bindgen_futures::JsFuture::from(wasm_bindgen_rayon::init_thread_pool(threads))
        .await
        .map_err(|error| format!("could not initialize the Web Worker pool: {error:?}"))?;

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .map_err(|error| format!("failed to create the browser event loop: {error}"))?;
    let proxy = event_loop.create_proxy();
    let app = App::web_new(proxy).map_err(|error| format!("failed to create the application state: {error:#}"))?;

    event_loop.spawn_app(app);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn show_web_startup_ready() {
    if let Some(window) = web_sys::window()
        && let Ok(handler) = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("inclineDesignStartupReady"))
        && let Some(handler) = wasm_bindgen::JsCast::dyn_ref::<js_sys::Function>(&handler)
    {
        let _ = handler.call0(&wasm_bindgen::JsValue::UNDEFINED);
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn show_web_startup_error(message: &str) {
    use wasm_bindgen::{JsCast, JsValue};
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Ok(handler) = js_sys::Reflect::get(&window, &JsValue::from_str("inclineDesignStartupError"))
        && let Some(handler) = handler.dyn_ref::<js_sys::Function>()
    {
        let _ = handler.call1(&JsValue::UNDEFINED, &JsValue::from_str(message));
    }
}
