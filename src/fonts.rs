//! Bundled fonts shared by the UI, document text, and plot renderer.

use std::sync::Arc;

use cosmic_text::{FontSystem, fontdb};

pub(crate) const NOTO_SANS: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSans-VF.ttf");
pub(crate) const NOTO_SANS_MONO: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansMono-VF.ttf");
pub(crate) const NOTO_SANS_ARABIC: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansArabic-VF.ttf");
pub(crate) const NOTO_SANS_DEVANAGARI: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansDevanagari-VF.ttf");
pub(crate) const NOTO_SANS_SC: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansSC-VF.ttf");
pub(crate) const NOTO_SANS_KR: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansKR-VF.ttf");
pub(crate) const NOTO_SANS_SYMBOLS: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansSymbols-VF.ttf");
pub(crate) const NOTO_SANS_SYMBOLS_2: &[u8] = include_bytes!("../res/fonts/noto-sans/NotoSansSymbols2-Regular.ttf");

/// Create a platform-independent text system with every script supported by
/// Incline. Keep this list in sync with the egui families in `ui::fonts`.
///
/// `NOTO_SANS_KR` covers Hangul; `NOTO_SANS_SC` covers Han ideographs for both
/// Chinese and Japanese.
pub(crate) fn cosmic_text_font_system() -> FontSystem {
    let sources = [
        NOTO_SANS,
        NOTO_SANS_MONO,
        NOTO_SANS_ARABIC,
        NOTO_SANS_DEVANAGARI,
        NOTO_SANS_SC,
        NOTO_SANS_KR,
        NOTO_SANS_SYMBOLS,
        NOTO_SANS_SYMBOLS_2,
    ]
    .map(|font| fontdb::Source::Binary(Arc::new(font)));
    let mut font_system = FontSystem::new_with_fonts(sources);

    // cosmic-text currently defaults its generic sans-serif family to Open
    // Sans. Incline intentionally bundles Noto Sans instead.
    font_system.db_mut().set_sans_serif_family("Noto Sans");
    font_system
}
