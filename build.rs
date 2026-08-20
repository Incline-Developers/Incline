fn main() {
    // Generate Windows ICO
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("res/logo.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
