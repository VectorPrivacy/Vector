//! Human-readable `.i2p` names, resolved through the router's own address book and remembered
//! for an hour. Per instance, so a new router connection starts with nothing cached.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use web_time::Instant;

const TTL: Duration = Duration::from_secs(3600);
const CAP: usize = 256;

#[derive(Default)]
pub struct NameCache {
    map: Mutex<HashMap<String, (String, Instant)>>,
}

impl NameCache {
    pub fn get(&self, name: &str) -> Option<String> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(name).filter(|(_, at)| at.elapsed() < TTL).map(|(v, _)| v.clone())
    }

    pub fn put(&self, name: &str, dest: String) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= CAP && !map.contains_key(name) {
            map.retain(|_, (_, at)| at.elapsed() < TTL);
            if map.len() >= CAP {
                if let Some(oldest) = map.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                    map.remove(&oldest);
                }
            }
        }
        map.insert(name.to_string(), (dest, Instant::now()));
    }

    /// A cached destination the router no longer accepts.
    pub fn forget(&self, name: &str) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
    }

    #[cfg(test)]
    pub(crate) fn age_for_test(&self, by: Duration) {
        for (_, at) in self.map.lock().unwrap_or_else(|e| e.into_inner()).values_mut() {
            *at = at.checked_sub(by).unwrap_or(*at);
        }
    }
}
