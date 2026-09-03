//! Translation catalog: the Fluent message bundles, the active-language loader,
//! and the [`tr!`] macro the UI uses in place of string literals.
//!
//! # How it fits together
//!
//! - Strings live in `i18n/<lang>/Incline.ftl` — the file is named after the
//!   cargo package (`CARGO_PKG_NAME`), so renaming the package means renaming
//!   these files. **English (`i18n/en`) is canonical** — every message id and
//!   every argument the code uses is checked against it at compile time by
//!   [`i18n_embed_fl::fl!`]. Other languages may be incomplete; a missing
//!   message falls back to English.
//! - The `.ftl` files are embedded in the binary at build time (`rust-embed`), so
//!   nothing is read from disk and the wasm build needs no extra bundling step —
//!   same model as the fonts in [`crate::ui::fonts`].
//! - The active language is resolved **once**, in [`init`], from the persisted
//!   [`LanguageChoice`] (see [`crate::app::io::Config`]). Changing the language in
//!   Preferences rewrites the config but does **not** take effect until the next
//!   launch — there is deliberately no runtime re-selection or font reload here.
//!
//! # Adding a string
//!
//! Add `my-message = English text` to `i18n/en/Incline.ftl` (and ideally
//! the other languages), then call `tr!("my-message")` where the literal was.
//! For interpolated values: `greeting = Hello, { $name }` → `tr!("greeting", name = who)`.
//!
//! See `.claude/skills/incline-i18n/SKILL.md` for the full recipe and the
//! migration checklist.

use std::sync::LazyLock;

use i18n_embed::{
    LanguageLoader,
    fluent::{FluentLanguageLoader, fluent_language_loader},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use unic_langid::{LanguageIdentifier, langid};

/// The embedded `i18n/` tree (`<lang>/Incline.ftl`).
#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

/// Process-wide Fluent loader. Loads the English fallback bundle on first use;
/// [`init`] then negotiates the user's language onto it.
pub(crate) static LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();
    loader.load_fallback_language(&Localizations).expect("i18n: the English fallback bundle must load");
    // Fluent isolates interpolated values with Unicode FSI/PDI control marks by
    // default; egui renders those as tofu, so switch the isolation off.
    loader.set_use_isolating(false);
    loader
});

/// UI language, as stored in `config.toml`.
///
/// `Copy` so it can live in `PreferencesDraft`. Serialises to a short tag
/// (`"system"`, `"en"`, `"ru"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LanguageChoice {
    /// Follow the operating-system locale, falling back to English.
    #[default]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ru")]
    Russian,
}

impl LanguageChoice {
    /// Every value, in the order the Preferences combo lists them.
    pub(crate) const ALL: [Self; 3] = [Self::System, Self::English, Self::Russian];

    /// Label for the Preferences combo. Each language names itself in its own
    /// script (an endonym); `System` is translated like any other string.
    pub(crate) fn combo_label(self) -> String {
        match self {
            Self::System => tr!("settings-language-system"),
            Self::English => "English".to_owned(),
            Self::Russian => "Русский".to_owned(),
        }
    }

    /// The language id this choice pins, or `None` when it defers to the OS.
    fn explicit_lang_id(self) -> Option<LanguageIdentifier> {
        match self {
            Self::System => None,
            Self::English => Some(langid!("en")),
            Self::Russian => Some(langid!("ru")),
        }
    }
}

/// Resolve the active language and install it on [`LOADER`].
///
/// Call once at startup, after the config is loaded and before the first frame
/// (the UI reads [`LOADER`] on every draw).
pub(crate) fn init(choice: LanguageChoice) {
    let requested: Vec<LanguageIdentifier> = match choice.explicit_lang_id() {
        Some(id) => vec![id],
        None => system_languages(),
    };
    if let Err(error) = i18n_embed::select(&*LOADER, &Localizations, &requested) {
        // Not fatal: the English fallback bundle is already loaded.
        log::warn!("i18n: could not select a language: {error}");
    }
    log::info!("i18n: active language is {} (bundled: {:?})", LOADER.current_language(), available_languages());
}

/// Available `<lang>` bundles, for logging / diagnostics.
pub(crate) fn available_languages() -> Vec<LanguageIdentifier> {
    LOADER.available_languages(&Localizations).unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn system_languages() -> Vec<LanguageIdentifier> {
    use i18n_embed::LanguageRequester;
    i18n_embed::DesktopLanguageRequester::new().requested_languages()
}

#[cfg(target_arch = "wasm32")]
fn system_languages() -> Vec<LanguageIdentifier> {
    // Phase 1 does not negotiate the browser locale; an empty list makes
    // `i18n_embed::select` keep the English fallback.
    Vec::new()
}

/// Translate a message id, with optional named Fluent arguments.
///
/// ```ignore
/// tr!("menu-file");
/// tr!("tri-count-polylines", count = n);
/// ```
///
/// Thin wrapper over [`i18n_embed_fl::fl!`], which checks the id and arguments
/// against `i18n/en/Incline.ftl` at compile time — a typo will not build.
macro_rules! tr {
    ($($tail:tt)*) => {
        i18n_embed_fl::fl!($crate::i18n::LOADER, $($tail)*)
    };
}
pub(crate) use tr;
