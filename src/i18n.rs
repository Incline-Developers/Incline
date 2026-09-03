//! Translation catalog: the Fluent message bundles, the active-language loader,
//! and the [`tr!`] macro the UI uses in place of string literals.
//!
//! # How it fits together
//!
//! - Strings live in `i18n/<lang>/Incline_Design.ftl` — the file is named after
//!   the cargo package (`CARGO_PKG_NAME`), so renaming the package means renaming
//!   these files. **English (`i18n/en`) is canonical** — every message id and
//!   every argument the code uses is checked against it at compile time by
//!   [`i18n_embed_fl::fl!`]. Other languages may be incomplete; a missing
//!   message falls back to English.
//! - The `.ftl` files are embedded in the binary at build time (`rust-embed`), so
//!   nothing is read from disk and the wasm build needs no extra bundling step —
//!   same model as the fonts in [`crate::ui::fonts`].
//! - The active language is a [`LanguageChoice`] persisted in the config (see
//!   [`crate::app::io::Config`]) and installed on [`LOADER`] by
//!   [`select_language`]. There is no "follow the system" state: the OS locale
//!   is consulted once, by [`LanguageChoice::default`], to seed the config on
//!   first launch, and from then on the stored choice is what runs.
//! - Switching is **live**. egui rebuilds the interface from scratch every
//!   frame, so re-selecting the language and asking for a redraw is all it takes
//!   — nothing caches a translated string across frames, and no restart is
//!   needed. The picker lives in the status bar
//!   ([`crate::ui::elements::status_bar`]).
//!
//! # Adding a string
//!
//! Add `my-message = English text` to `i18n/en/Incline_Design.ftl` (and ideally
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

/// The embedded `i18n/` tree (`<lang>/Incline_Design.ftl`).
#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

/// Process-wide Fluent loader. Loads the English fallback bundle on first use;
/// [`select_language`] then installs the language the user is running.
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
/// (`"en"`, `"ru"`). Every value names a bundle in `i18n/`; there is
/// deliberately no "system" value — see [`Self::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LanguageChoice {
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ru")]
    Russian,
}

impl Default for LanguageChoice {
    /// The OS locale, or English when it names no language we bundle.
    ///
    /// This is what a missing `language` key in the config resolves to, so the
    /// system locale decides the language on the first launch and the stored
    /// choice decides it on every launch after that.
    fn default() -> Self {
        Self::from_system_locale()
    }
}

impl LanguageChoice {
    /// Every value, in the order the status bar's picker lists them.
    pub(crate) const ALL: [Self; 2] = [Self::English, Self::Russian];

    /// How this language names itself, in its own script. Never translated:
    /// someone who cannot read the running language has to be able to find
    /// their own in the list.
    pub(crate) fn endonym(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
        }
    }

    /// The bundle this choice selects.
    fn lang_id(self) -> LanguageIdentifier {
        match self {
            Self::English => langid!("en"),
            Self::Russian => langid!("ru"),
        }
    }

    /// The first bundled language the OS asks for, ignoring region and script:
    /// `ru-RU` and `ru` both pick Russian.
    fn from_system_locale() -> Self {
        for requested in system_languages() {
            if let Some(choice) = Self::ALL.into_iter().find(|choice| choice.lang_id().language == requested.language) {
                return choice;
            }
        }
        Self::English
    }
}

/// Install `choice` on [`LOADER`].
///
/// Called at startup with the config's language and again on every switch from
/// the status bar's picker. Safe to call mid-session: the caller only has to
/// ask for a redraw, since the next frame rebuilds every string.
pub(crate) fn select_language(choice: LanguageChoice) {
    if let Err(error) = i18n_embed::select(&*LOADER, &Localizations, &[choice.lang_id()]) {
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
    // The browser locale is not negotiated yet, so the web build's first launch
    // lands on English; the picker still switches it from there.
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
/// against `i18n/en/Incline_Design.ftl` at compile time — a typo will not build.
macro_rules! tr {
    ($($tail:tt)*) => {
        i18n_embed_fl::fl!($crate::i18n::LOADER, $($tail)*)
    };
}
pub(crate) use tr;
