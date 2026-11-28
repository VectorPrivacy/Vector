//! A language against English: what is missing, obsolete or broken, and its coverage.

use std::collections::{BTreeMap, HashMap};

use crate::scan::{self, Kind, Unit};

#[derive(Debug, Default)]
pub struct Report {
    /// Messages and attributes English has.
    pub total: usize,
    /// Of those, how many this language translates without fault.
    pub translated: usize,
    /// In English, not translated yet.
    pub missing: Vec<String>,
    /// Translated, but English no longer has them. Never shipped.
    pub obsolete: Vec<String>,
    /// `(id or file:line, why)`: each falls back to English until fixed.
    pub broken: Vec<(String, String)>,
}

impl Report {
    /// Translated share of English, as a whole percentage rounded down, so 100 means every message.
    pub fn coverage(&self) -> u32 {
        if self.total == 0 { 100 } else { (self.translated * 100 / self.total) as u32 }
    }

    /// [`coverage`](Self::coverage), except that an English variant counts what it leaves to
    /// en-GB as done.
    pub fn coverage_for(&self, tag: &str) -> u32 {
        if !crate::inherits_english(tag) || self.total == 0 {
            return self.coverage();
        }
        let broken = self.broken.iter().filter(|(id, _)| !id.ends_with(".ftl")).count();
        (self.total.saturating_sub(broken) * 100 / self.total) as u32
    }
}

fn english() -> Vec<Unit> {
    let mut units: Vec<Unit> = crate::EN_GB.iter().flat_map(|(f, s)| scan::scan(f, s).units).collect();
    scan::resolve(&mut units);
    units
}

/// Checks a language's files, `(file name, source)`, against the built-in en-GB.
pub fn check(files: &[(String, String)]) -> Report {
    let en = english();
    let mut report = Report { total: en.len(), ..Default::default() };
    let mut ours: Vec<Unit> = vec![];
    for (f, s) in files {
        let sc = scan::scan(f, s);
        for e in sc.errors {
            report.broken.push((f.clone(), e));
        }
        ours.extend(sc.units);
    }
    scan::resolve(&mut ours);
    let ours: HashMap<&str, &Unit> = ours.iter().map(|u| (u.id.as_str(), u)).collect();
    let en_ids: HashMap<&str, &Unit> = en.iter().map(|u| (u.id.as_str(), u)).collect();
    for u in &en {
        let Some(t) = ours.get(u.id.as_str()) else {
            report.missing.push(u.id.clone());
            continue;
        };
        match fault(u, t) {
            Some(why) => report.broken.push((u.id.clone(), why)),
            None => report.translated += 1,
        }
    }
    let mut obsolete: Vec<String> = ours.keys().filter(|id| !en_ids.contains_key(*id)).map(|s| s.to_string()).collect();
    obsolete.sort();
    report.obsolete = obsolete;
    report
}

/// Why a translation of `en` can't be used as it stands.
fn fault(en: &Unit, t: &Unit) -> Option<String> {
    let unknown: Vec<&String> = t.vars.keys().filter(|v| !en.vars.contains_key(*v)).collect();
    if !unknown.is_empty() {
        let names: Vec<String> = unknown.iter().map(|v| format!("${v}")).collect();
        return Some(format!("uses {}, which English doesn't provide", names.join(", ")));
    }
    let lists = |vars: &BTreeMap<String, Kind>| vars.iter().filter(|(_, k)| **k == Kind::List).map(|(v, _)| v.clone()).collect::<Vec<_>>();
    if lists(&t.vars).iter().any(|v| en.vars.get(v) != Some(&Kind::List)) {
        return Some("passes a value to LIST that English doesn't".into());
    }
    let (mut a, mut b) = (en.tags.clone(), t.tags.clone());
    a.sort();
    b.sort();
    if a != b {
        return Some(format!("tags differ from English: expected {:?}, found {:?}", a, b));
    }
    None
}
