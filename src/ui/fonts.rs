//! Noto Sans setup and bold text helpers.

use crate::fonts::{NOTO_SANS, NOTO_SANS_ARABIC, NOTO_SANS_DEVANAGARI, NOTO_SANS_MONO, NOTO_SANS_SC, NOTO_SANS_SYMBOLS, NOTO_SANS_SYMBOLS_2};

const REGULAR_WEIGHT: f32 = 400.0;
const BOLD_WEIGHT: f32 = 700.0;

fn variable_font(font: &'static [u8], weight: f32) -> egui::FontData {
    egui::FontData::from_static(font).tweak(egui::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]),
        ..Default::default()
    })
}

/// Install the bundled Noto Sans fonts into the egui context.
///
/// The base and mono faces cover Latin and Cyrillic. Script-specific faces are
/// fallbacks for Simplified Chinese, Arabic/Persian, Devanagari, and UI
/// symbols. The text faces are variable, so regular and bold share files.
pub(crate) fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::empty();
    for (name, data) in [
        ("noto_sans_regular", variable_font(NOTO_SANS, REGULAR_WEIGHT)),
        ("noto_sans_mono_regular", variable_font(NOTO_SANS_MONO, REGULAR_WEIGHT)),
        ("noto_sans_arabic_regular", variable_font(NOTO_SANS_ARABIC, REGULAR_WEIGHT)),
        ("noto_sans_devanagari_regular", variable_font(NOTO_SANS_DEVANAGARI, REGULAR_WEIGHT)),
        ("noto_sans_sc_regular", variable_font(NOTO_SANS_SC, REGULAR_WEIGHT)),
        ("noto_sans_symbols_regular", variable_font(NOTO_SANS_SYMBOLS, REGULAR_WEIGHT)),
        ("noto_sans_symbols_2", egui::FontData::from_static(NOTO_SANS_SYMBOLS_2)),
        ("noto_sans_bold", variable_font(NOTO_SANS, BOLD_WEIGHT)),
        ("noto_sans_arabic_bold", variable_font(NOTO_SANS_ARABIC, BOLD_WEIGHT)),
        ("noto_sans_devanagari_bold", variable_font(NOTO_SANS_DEVANAGARI, BOLD_WEIGHT)),
        ("noto_sans_sc_bold", variable_font(NOTO_SANS_SC, BOLD_WEIGHT)),
        ("noto_sans_symbols_bold", variable_font(NOTO_SANS_SYMBOLS, BOLD_WEIGHT)),
    ] {
        fonts.font_data.insert(name.to_owned(), data.into());
    }

    let script_fallbacks = [
        "noto_sans_regular",
        "noto_sans_arabic_regular",
        "noto_sans_devanagari_regular",
        "noto_sans_sc_regular",
        "noto_sans_symbols_regular",
        "noto_sans_symbols_2",
    ];
    fonts
        .families
        .insert(egui::FontFamily::Proportional, script_fallbacks.into_iter().map(str::to_owned).collect());
    fonts.families.insert(
        egui::FontFamily::Monospace,
        [
            "noto_sans_mono_regular",
            "noto_sans_arabic_regular",
            "noto_sans_devanagari_regular",
            "noto_sans_sc_regular",
            "noto_sans_symbols_regular",
            "noto_sans_symbols_2",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
    fonts.families.insert(
        egui::FontFamily::Name("noto_sans_bold".into()),
        [
            "noto_sans_bold",
            "noto_sans_arabic_bold",
            "noto_sans_devanagari_bold",
            "noto_sans_sc_bold",
            "noto_sans_symbols_bold",
            "noto_sans_symbols_2",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
    ctx.set_fonts(fonts);
}

/// Return a [`RichText`] styled with the bundled bold font face.
pub(crate) fn bold(label: &str) -> egui::RichText {
    egui::RichText::new(label).family(egui::FontFamily::Name("noto_sans_bold".into()))
}
