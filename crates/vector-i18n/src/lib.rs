//! Vector's app text. British English is compiled in; every other language is a pack added at
//! run time. Lookups go through typed functions that `define_t!` generates from the en-GB
//! files, so a removed message or a missing value is a compile error.
//!
//! A message falls back on its own: the chosen language, then its parent (`pt-BR` → `pt`),
//! then en-GB. One missing or broken translation never blanks a screen.

mod check;
mod dates;
pub mod index;
mod numbers;
mod pack;
mod pseudo;
#[allow(dead_code)]
mod scan;

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use fluent_bundle::{FluentArgs, FluentResource, FluentValue, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

pub use check::{Report, check};
pub use dates::{Civil, Clock, DateStyle, Dates};
pub use numbers::LocaleData;
pub use pack::{PACK_FORMAT, english_revision};

include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

/// The lookups as `Arc<str>`, for code with no string type of its own.
pub mod t {
    define_t!(std::sync::Arc<str>);
}

pub const BUILT_IN: &str = "en-GB";
/// Accented and stretched English, for spotting untranslated and clipped text.
pub const PSEUDO_ACCENTED: &str = "en-XA";
/// Message ids in place of text, for translator screenshots.
pub const PSEUDO_IDS: &str = "en-XI";
/// English laid out right to left, for checking mirrored screens before a real RTL language.
pub const PSEUDO_MIRRORED: &str = "ar-XB";

/// A language this build knows the repository translates, for offering it before anything is
/// downloaded. The download is what's checked; this is only a name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offered {
    pub tag: &'static str,
    pub name: &'static str,
    pub english_name: &'static str,
    /// Roughly, when this build was made.
    pub coverage: u32,
}

/// A language added at run time.
#[derive(Clone, Debug)]
pub struct Pack {
    /// BCP 47, such as `es` or `pt-BR`.
    pub tag: String,
    /// In its own language: `Español`.
    pub name: String,
    /// In English: `Spanish`. Empty when the pack doesn't say.
    pub english_name: String,
    pub rtl: bool,
    /// `(file name, source)`, mirroring the en-GB files.
    pub files: Vec<(String, String)>,
    pub data: LocaleData,
    /// The [`english_revision`] it was built against.
    pub english: String,
    /// Translated share of that English, 0 to 100.
    pub coverage: u32,
}

impl Pack {
    /// A folder of `.ftl` files, as a translator keeps them, with the language's number and
    /// date data in an optional `locale.json`.
    pub fn from_dir(tag: &str, dir: &std::path::Path) -> std::io::Result<Self> {
        let mut files = vec![];
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "ftl") {
                files.push((p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p)?));
            }
        }
        files.sort();
        let data = match std::fs::read(dir.join("locale.json")) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("locale.json: {e}")))?,
            Err(_) => LocaleData::default(),
        };
        let lang = Lang::new(tag, &files, LocaleData::default(), false);
        let name = lang.format(NAME_ID, None).unwrap_or_else(|| tag.to_string());
        let english_name = lang.format(ENGLISH_NAME_ID, None).unwrap_or_default();
        let coverage = check(&files).coverage_for(tag);
        Ok(Self { tag: tag.to_string(), name, english_name, rtl: is_rtl_tag(tag), files, data, english: english_revision(), coverage })
    }
}

fn is_rtl_tag(tag: &str) -> bool {
    matches!(tag.split('-').next().unwrap_or(""), "ar" | "he" | "fa" | "ur" | "ps" | "sd" | "ug" | "yi" | "dv" | "ckb")
}

struct Lang {
    bundle: FluentBundle<FluentResource>,
}

impl Lang {
    fn new(tag: &str, files: &[(impl AsRef<str>, impl AsRef<str>)], data: LocaleData, accent: bool) -> Self {
        let id: LanguageIdentifier = tag.parse().unwrap_or_default();
        let mut bundle = FluentBundle::new_concurrent(vec![id]);
        for (_, src) in files {
            // A file that fails to parse keeps what did parse; the rest falls back.
            let res = FluentResource::try_new(src.as_ref().to_string()).unwrap_or_else(|(res, _)| res);
            let _ = bundle.add_resource(res);
        }
        if accent {
            bundle.set_transform(Some(pseudo::accent));
        }
        let data = Arc::new(data);
        numbers::register(tag, data.clone());
        numbers::install(&mut bundle, data);
        Self { bundle }
    }

    fn pattern(&self, id: &str) -> Option<&fluent_syntax::ast::Pattern<&str>> {
        let (msg, attr) = match id.split_once('.') {
            Some((m, a)) => (m, Some(a)),
            None => (id, None),
        };
        let m = self.bundle.get_message(msg)?;
        match attr {
            Some(a) => Some(m.get_attribute(a)?.value()),
            None => m.value(),
        }
    }

    /// `None` when it lacks the message or a value it needs, or the translation is broken.
    fn format(&self, id: &str, args: Option<&FluentArgs>) -> Option<String> {
        let p = self.pattern(id)?;
        let mut errors = vec![];
        let s = self.bundle.format_pattern(p, args, &mut errors);
        errors.is_empty().then(|| clean(s))
    }
}

/// Isolation marks keep a right-to-left value's punctuation in place; text with none in it
/// doesn't need them.
fn clean(s: Cow<str>) -> String {
    if s.contains(['\u{2068}', '\u{2069}']) && !has_rtl(&s) {
        return s.chars().filter(|c| !matches!(c, '\u{2068}' | '\u{2069}')).collect();
    }
    s.into_owned()
}

fn has_rtl(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF))
}

#[derive(Clone, Copy, PartialEq)]
enum Pseudo {
    None,
    Accented,
    Ids,
    Mirrored,
}

struct Active {
    tag: String,
    rtl: bool,
    pseudo: Pseudo,
    data: Arc<LocaleData>,
    /// Most specific first; en-GB last.
    chain: Vec<Arc<Lang>>,
    fixed: Box<[Arc<str>]>,
}

impl Active {
    fn build(tag: &str, chain: Vec<Arc<Lang>>, rtl: bool, pseudo: Pseudo) -> Self {
        let mut data = numbers::data_for(tag);
        let (system, hour12) = SYSTEM.read().unwrap().clone();
        let english = tag == BUILT_IN || pseudo != Pseudo::None;
        if english || hour12.is_some() {
            let d = Arc::make_mut(&mut data);
            if english {
                d.dates = dates::english_for(&system);
            }
            if let Some(h) = hour12 {
                d.dates.hour12 = h;
            }
        }
        let mut a = Self { tag: tag.to_string(), rtl, pseudo, data, chain, fixed: Box::new([]) };
        a.fixed = IDS
            .iter()
            .zip(FIXED)
            .map(|(id, fixed)| if *fixed { Arc::from(a.resolve(id, None)) } else { Arc::from("") })
            .collect();
        a
    }

    fn resolve(&self, id: &str, args: Option<&FluentArgs>) -> String {
        if self.pseudo == Pseudo::Ids {
            return id.to_string();
        }
        let mut translated = false;
        let last = self.chain.len() - 1;
        let s = self.chain.iter().enumerate().find_map(|(i, l)| {
            let s = l.format(id, args)?;
            translated = i < last;
            Some(s)
        });
        let s = s.unwrap_or_else(|| {
            // Even English failed (a value the code didn't pass): show what it could.
            let mut errors = vec![];
            let l = self.chain.last().unwrap();
            l.pattern(id).map(|p| clean(l.bundle.format_pattern(p, args, &mut errors))).unwrap_or_else(|| id.to_string())
        });
        if self.pseudo == Pseudo::Accented {
            return pseudo::stretch(&s);
        }
        // A translation opening with a Latin word or a value still reads right to left, as
        // a paragraph takes its first letter's direction.
        if translated && self.rtl && has_rtl(&s) && first_strong_rtl(&s) != Some(true) {
            return format!("\u{200F}{s}");
        }
        s
    }
}

/// UAX #9 P2: the first strong letter's direction, skipping isolated values and markup.
fn first_strong_rtl(s: &str) -> Option<bool> {
    let (mut isolated, mut tag) = (0u32, false);
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            '\u{2066}'..='\u{2068}' => isolated += 1,
            '\u{2069}' => isolated = isolated.saturating_sub(1),
            _ if tag || isolated > 0 => {}
            '\u{200F}' => return Some(true),
            '\u{200E}' => return Some(false),
            _ if c.is_alphabetic() => return Some(has_rtl(c.encode_utf8(&mut [0; 4]))),
            _ => {}
        }
    }
    None
}

struct Registry {
    packs: Vec<Arc<Pack>>,
    langs: Vec<(String, Arc<Lang>)>,
}

static EN_GB_LANG: LazyLock<Arc<Lang>> = LazyLock::new(|| Arc::new(Lang::new(BUILT_IN, EN_GB, LocaleData::default(), false)));
static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(Registry { packs: vec![], langs: vec![] }));
static ACTIVE: LazyLock<RwLock<Arc<Active>>> =
    LazyLock::new(|| RwLock::new(Arc::new(Active::build(BUILT_IN, vec![EN_GB_LANG.clone()], false, Pseudo::None))));
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The system's locale tag, and its 12 or 24-hour choice when it makes one.
static SYSTEM: RwLock<(String, Option<bool>)> = RwLock::new((String::new(), None));

thread_local! {
    static CURRENT: RefCell<Option<(u64, Arc<Active>)>> = const { RefCell::new(None) };
}

/// `f` runs outside the thread's cache borrow, so a lookup inside it can't panic.
fn with_active<R>(f: impl FnOnce(&Active) -> R) -> R {
    let generation = GENERATION.load(Ordering::Acquire);
    let active = CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_none_or(|(g, _)| *g != generation) {
            *c = Some((generation, ACTIVE.read().unwrap().clone()));
        }
        c.as_ref().unwrap().1.clone()
    });
    f(&active)
}

/// The message a language names itself with, which the language list reads.
pub const NAME_ID: &str = "language-name";
/// Its name in English, for someone looking for it in a list they can't read.
pub const ENGLISH_NAME_ID: &str = "language-name-english";

/// English variants inherit en-GB: a message they leave out already reads right.
pub fn inherits_english(tag: &str) -> bool {
    tag.split('-').next() == Some("en")
}

/// Adds or replaces a pack. Takes effect on the next [`set_language`] for its tag; the
/// generation moves so a cached language list is rebuilt.
pub fn install(pack: Pack) {
    let mut r = REGISTRY.write().unwrap();
    r.langs.retain(|(t, _)| *t != pack.tag);
    r.packs.retain(|p| p.tag != pack.tag);
    r.packs.push(Arc::new(pack));
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// A language that can be chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct Language {
    pub tag: String,
    /// In its own language.
    pub name: String,
    /// In English; empty when the pack doesn't say.
    pub english_name: String,
    pub coverage: u32,
}

/// The languages that can be chosen: en-GB, then every installed pack by name.
pub fn languages() -> Vec<Language> {
    let r = REGISTRY.read().unwrap();
    let english = EN_GB_LANG.format(NAME_ID, None).unwrap_or_else(|| BUILT_IN.to_string());
    let english_name = EN_GB_LANG.format(ENGLISH_NAME_ID, None).unwrap_or_else(|| english.clone());
    let mut packs: Vec<Language> = r
        .packs
        .iter()
        .map(|p| Language { tag: p.tag.clone(), name: p.name.clone(), english_name: p.english_name.clone(), coverage: p.coverage })
        .collect();
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    std::iter::once(Language { tag: BUILT_IN.to_string(), name: english, english_name, coverage: 100 }).chain(packs).collect()
}

fn lang(r: &mut Registry, tag: &str) -> Option<Arc<Lang>> {
    if let Some((_, l)) = r.langs.iter().find(|(t, _)| t == tag) {
        return Some(l.clone());
    }
    let p = r.packs.iter().find(|p| p.tag == tag)?.clone();
    let l = Arc::new(Lang::new(&p.tag, &p.files, p.data.clone(), false));
    r.langs.push((p.tag.clone(), l.clone()));
    Some(l)
}

/// Switches every lookup to `tag`. Unknown tags fall back to en-GB; returns whether `tag`
/// (or a parent of it) was found.
pub fn set_language(tag: &str) -> bool {
    let (chain, rtl, pseudo, found) = match tag {
        PSEUDO_ACCENTED => (vec![Arc::new(Lang::new(tag, EN_GB, LocaleData::default(), true))], false, Pseudo::Accented, true),
        PSEUDO_IDS => (vec![EN_GB_LANG.clone()], false, Pseudo::Ids, true),
        PSEUDO_MIRRORED => (vec![EN_GB_LANG.clone()], true, Pseudo::Mirrored, true),
        _ => {
            let mut r = REGISTRY.write().unwrap();
            let mut chain = vec![];
            let mut t = tag;
            loop {
                if t == BUILT_IN {
                    break;
                }
                if let Some(l) = lang(&mut r, t) {
                    chain.push(l);
                }
                match t.rsplit_once('-') {
                    Some((parent, _)) => t = parent,
                    None => break,
                }
            }
            let found = !chain.is_empty() || tag == BUILT_IN;
            let rtl = r.packs.iter().find(|p| p.tag == tag).map_or(false, |p| p.rtl);
            chain.push(EN_GB_LANG.clone());
            (chain, rtl, Pseudo::None, found)
        }
    };
    let tag = if found { tag } else { BUILT_IN };
    *ACTIVE.write().unwrap() = Arc::new(Active::build(tag, chain, rtl, pseudo));
    GENERATION.fetch_add(1, Ordering::AcqRel);
    found
}

/// The system's locale (`en-US`, `de_DE.UTF-8`) and clock, for what the language leaves to
/// the region: English's date order, and whether the clock runs to 12 or 24 hours.
pub fn set_system_locale(tag: &str, hour12: Option<bool>) {
    *SYSTEM.write().unwrap() = (tag.to_string(), hour12);
    set_language(&language());
}

/// Every message id the code can ask for (`id` or `id.attribute`), in catalogue order.
pub fn ids() -> &'static [&'static str] {
    IDS
}

/// The `t::` function a message id becomes: `call-start.tip` → `call_start_tip`.
pub fn function_name(id: &str) -> String {
    scan::fn_name(id)
}

/// The active language's tag.
pub fn language() -> String {
    with_active(|a| a.tag.clone())
}

/// Whether the active language reads right to left.
pub fn is_rtl() -> bool {
    with_active(|a| a.rtl)
}

/// Bumped on every switch, so a client can tell its caches are stale.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// The active language's number data, for formatting outside a message.
pub fn locale() -> Arc<LocaleData> {
    with_active(|a| a.data.clone())
}

#[doc(hidden)]
pub fn fixed(index: usize) -> Arc<str> {
    with_active(|a| a.fixed[index].clone())
}

#[doc(hidden)]
pub fn format(index: usize, args: &Args) -> String {
    with_active(|a| a.resolve(IDS[index], Some(&args.0)))
}

/// A value a message places: text, or a number (which plural forms select on).
pub enum Value<'a> {
    Text(Cow<'a, str>),
    Num(f64),
}

impl<'a, T: AsRef<str> + ?Sized> From<&'a T> for Value<'a> {
    fn from(s: &'a T) -> Self {
        Value::Text(Cow::Borrowed(s.as_ref()))
    }
}

impl From<String> for Value<'_> {
    fn from(s: String) -> Self {
        Value::Text(Cow::Owned(s))
    }
}

impl<'a> From<Cow<'a, str>> for Value<'a> {
    fn from(s: Cow<'a, str>) -> Self {
        Value::Text(s)
    }
}

/// A number a message selects on or formats.
#[derive(Clone, Copy)]
pub struct Num(pub f64);

macro_rules! numeric {
    ($($t:ty),*) => {$(
        impl From<$t> for Num { fn from(n: $t) -> Self { Num(n as f64) } }
        impl From<$t> for Value<'_> { fn from(n: $t) -> Self { Value::Num(n as f64) } }
    )*};
}
numeric!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64);

#[doc(hidden)]
pub struct Args<'a>(FluentArgs<'a>);

impl<'a> Args<'a> {
    pub fn with_capacity(n: usize) -> Self {
        Self(FluentArgs::with_capacity(n))
    }

    pub fn value(&mut self, name: &'static str, v: impl Into<Value<'a>>) {
        match v.into() {
            Value::Text(s) => self.0.set(name, FluentValue::String(s)),
            Value::Num(n) => self.0.set(name, numbers::plain(n)),
        }
    }

    pub fn num(&mut self, name: &'static str, n: impl Into<Num>) {
        self.0.set(name, numbers::plain(n.into().0));
    }

    pub fn list(&mut self, name: &'static str, items: &[impl AsRef<str>]) {
        let joined = with_active(|a| a.data.list(items));
        self.0.set(name, FluentValue::String(joined.into()));
    }
}

#[cfg(test)]
mod tests;
