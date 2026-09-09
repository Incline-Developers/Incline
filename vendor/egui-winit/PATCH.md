Vendored from egui-winit 0.36.2 on crates.io (MIT OR Apache-2.0).

Local change: compile NativeFile and enqueue native dropped files only on
non-wasm targets. NativeFile implements the synchronous DroppedFile::bytes
method, which is unavailable on wasm. Winit does not emit native path-based
file-drop events on the web backend.

Remove this patch and the Cargo.toml override once an upstream release fixes
WebAssembly compilation.
