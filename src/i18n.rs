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
//! For a short UI literal that has not yet received a hand-written id, use
//! `tr!(literal = "Apply")`; its stable `literal-*` id can be translated in
//! each catalog without changing the call site.
//!
//! See `.claude/skills/incline-i18n/SKILL.md` for the full recipe and the
//! migration checklist.

use std::{fmt::Write as _, sync::LazyLock};

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
/// (`"en"`, `"es"`, and so on). Every value names a bundle in `i18n/`; there is
/// deliberately no "system" value — see [`Self::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LanguageChoice {
    #[serde(rename = "en")]
    English,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "pt")]
    Portuguese,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "zh")]
    ChineseSimplified,
    #[serde(rename = "id")]
    Indonesian,
    #[serde(rename = "ru")]
    Russian,
    #[serde(rename = "ar")]
    Arabic,
    #[serde(rename = "fa")]
    Farsi,
    #[serde(rename = "hi")]
    Hindi,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "it")]
    Italian,
    #[serde(rename = "pl")]
    Polish,
    #[serde(rename = "tr")]
    Turkish,
    #[serde(rename = "vi")]
    Vietnamese,
    #[serde(rename = "mn")]
    Mongolian,
    #[serde(rename = "sw")]
    Swahili,
    #[serde(rename = "ja")]
    Japanese,
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "uk")]
    Ukrainian,
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
    pub(crate) const ALL: [Self; 20] = [
        Self::English,
        Self::Spanish,
        Self::Portuguese,
        Self::French,
        Self::ChineseSimplified,
        Self::Indonesian,
        Self::Russian,
        Self::Arabic,
        Self::Farsi,
        Self::Hindi,
        Self::German,
        Self::Italian,
        Self::Polish,
        Self::Turkish,
        Self::Vietnamese,
        Self::Mongolian,
        Self::Swahili,
        Self::Japanese,
        Self::Korean,
        Self::Ukrainian,
    ];

    /// How this language names itself, in its own script. Never translated:
    /// someone who cannot read the running language has to be able to find
    /// their own in the list.
    pub(crate) fn endonym(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Español",
            Self::Portuguese => "Português",
            Self::French => "Français",
            Self::ChineseSimplified => "简体中文",
            Self::Indonesian => "Bahasa Indonesia",
            Self::Russian => "Русский",
            Self::Arabic => "العربية",
            Self::Farsi => "فارسی",
            Self::Hindi => "हिन्दी",
            Self::German => "Deutsch",
            Self::Italian => "Italiano",
            Self::Polish => "Polski",
            Self::Turkish => "Türkçe",
            Self::Vietnamese => "Tiếng Việt",
            Self::Mongolian => "Монгол",
            Self::Swahili => "Kiswahili",
            Self::Japanese => "日本語",
            Self::Korean => "한국어",
            Self::Ukrainian => "Українська",
        }
    }

    /// The bundle this choice selects.
    fn lang_id(self) -> LanguageIdentifier {
        match self {
            Self::English => langid!("en"),
            Self::Spanish => langid!("es"),
            Self::Portuguese => langid!("pt"),
            Self::French => langid!("fr"),
            Self::ChineseSimplified => langid!("zh"),
            Self::Indonesian => langid!("id"),
            Self::Russian => langid!("ru"),
            Self::Arabic => langid!("ar"),
            Self::Farsi => langid!("fa"),
            Self::Hindi => langid!("hi"),
            Self::German => langid!("de"),
            Self::Italian => langid!("it"),
            Self::Polish => langid!("pl"),
            Self::Turkish => langid!("tr"),
            Self::Vietnamese => langid!("vi"),
            Self::Mongolian => langid!("mn"),
            Self::Swahili => langid!("sw"),
            Self::Japanese => langid!("ja"),
            Self::Korean => langid!("ko"),
            Self::Ukrainian => langid!("uk"),
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
        log::warn!("{}", crate::i18n::tr_format!(literal = "Could not select a language: %error%", error = error));
    }
    log::info!(
        "{}",
        crate::i18n::tr_format!(
            literal = "Active language is %language% (bundled: %bundled%)",
            language = LOADER.current_language(),
            bundled = format!("{:?}", available_languages())
        )
    );
}

/// Available `<lang>` bundles, for logging / diagnostics.
pub(crate) fn available_languages() -> Vec<LanguageIdentifier> {
    LOADER.available_languages(&Localizations).unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn system_languages() -> Vec<LanguageIdentifier> {
    sys_locale::get_locales().filter_map(|locale| parse_system_language(&locale)).collect()
}

/// Convert platform locale spellings such as `en_US.UTF-8` into Unicode
/// language identifiers. `C` and `POSIX` deliberately mean no language; the
/// caller will use English when they are the only configured locales.
#[cfg(not(target_arch = "wasm32"))]
fn parse_system_language(locale: &str) -> Option<LanguageIdentifier> {
    let locale = locale.split(['.', '@']).next().unwrap_or(locale);
    if locale.eq_ignore_ascii_case("c") || locale.eq_ignore_ascii_case("posix") {
        return None;
    }
    locale.replace('_', "-").parse().ok()
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
/// Translate a source literal that has not yet been assigned a hand-written
/// message id. The generated id is readable and includes a stable hash, so
/// punctuation and whitespace remain significant (`"Layer"` and `"Layer:"`
/// cannot accidentally share a translation). Missing entries intentionally
/// fall back to the source text while a locale is being filled.
pub(crate) fn tr_literal(source: &str) -> String {
    let id = literal_id(source);
    let translated = LOADER.get(&id);
    if !translated.starts_with("No localization for id:") {
        // Fluent trims padding around message values. A few compact labels are
        // deliberately fragments (" FPS", "Major "), so restore their source
        // padding after translation.
        let leading = source.len() - source.trim_start().len();
        let trailing = source.len() - source.trim_end().len();
        let content_end = source.len().saturating_sub(trailing);
        format!("{}{}{}", &source[..leading], translated.trim(), &source[content_end..])
    } else {
        source.to_owned()
    }
}

fn literal_id(source: &str) -> String {
    let mut id = String::from("literal-");
    let mut separator = false;
    for character in source.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && id.len() > "literal-".len() {
                id.push('-');
            }
            for lower in character.to_lowercase() {
                id.push(lower);
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    if id.ends_with('-') {
        id.pop();
    }
    if id == "literal-" {
        id.push_str("value");
    }

    // FNV-1a is intentionally fixed rather than DefaultHasher, whose output is
    // not a persistence contract. The catalog generator uses the same bytes.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    write!(&mut id, "-{hash:016x}").expect("writing to a String cannot fail");
    id
}

/// Thin wrapper over [`i18n_embed_fl::fl!`], which checks hand-written ids and
/// their arguments against `i18n/en/Incline_Design.ftl` at compile time.
macro_rules! tr {
    (literal = $source:literal) => {
        $crate::i18n::tr_literal($source)
    };
    ($($tail:tt)*) => {
        i18n_embed_fl::fl!($crate::i18n::LOADER, $($tail)*)
    };
}
pub(crate) use tr;

/// Translate a source literal and substitute named values written as
/// `%name%` placeholders.
///
/// This is the literal counterpart of Fluent's normal named arguments. It is
/// intentionally limited to simple textual replacement so ad-hoc activity
/// messages can be catalogued without making their source strings invalid
/// Fluent syntax. Values are inserted after lookup, so translators may move
/// each placeholder to suit their language.
///
/// ```ignore
/// tr_format!(literal = "Loading %name%…", name = filename)
/// ```
macro_rules! tr_format {
    (literal = $source:literal, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut translated = $crate::i18n::tr_literal($source);
        $(
            translated = translated.replace(concat!("%", stringify!($name), "%"), &::std::string::ToString::to_string(&($value)));
        )+
        translated
    }};
}
pub(crate) use tr_format;
