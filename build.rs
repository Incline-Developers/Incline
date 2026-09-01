fn main() {
    // Without declared inputs cargo reruns this script, and with it the whole
    // bin crate, whenever any file in the package changes (even index.html).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/logo.ico");

    // Generate Windows ICO
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("res/logo.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
