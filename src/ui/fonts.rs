//! Custom font setup and bold text helpers.

/// Install the bundled Open Sans fonts into the egui context.
///
/// Registers `open_sans_regular` and `open_sans_bold`, maps them into
/// the `Proportional` and `Monospace` families, and creates a dedicated
/// `open_sans_bold` family name.
pub(crate) fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "open_sans_regular".to_owned(),
        egui::FontData::from_static(include_bytes!("../../res/fonts/open-sans/OpenSans-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "open_sans_bold".to_owned(),
        egui::FontData::from_static(include_bytes!("../../res/fonts/open-sans/OpenSans-Bold.ttf")).into(),
    );
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "open_sans_regular".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("open_sans_regular".to_owned());
    fonts.families.insert(egui::FontFamily::Name("open_sans_bold".into()), vec!["open_sans_bold".to_owned()]);
    ctx.set_fonts(fonts);
}

/// Return a [`RichText`] styled with the bundled bold font face.
pub(crate) fn bold(label: &str) -> egui::RichText {
    egui::RichText::new(label).family(egui::FontFamily::Name("open_sans_bold".into()))
}
