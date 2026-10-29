//! Hash maps for keys a peer can choose (edition and event ids, npubs) that live within one
//! computation, as a fold's do: foldhash's speed, seeded with a secret from the OS. foldhash seeds
//! itself from addresses and the clock, which a wasm build leaves fixed, so a precomputed set of
//! colliding ids would slow every reader.

use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use foldhash::fast::{FoldHasher, SeedableRandomState};
use foldhash::SharedSeed;

static SECRET: OnceLock<SharedSeed> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A map's hasher: the process's secret, and a seed of the map's own.
#[derive(Clone, Debug)]
pub struct Seeded(SeedableRandomState);

impl Default for Seeded {
    fn default() -> Self {
        let secret = SECRET.get_or_init(|| SharedSeed::from_u64(rand::RngCore::next_u64(&mut rand::rngs::OsRng)));
        Seeded(SeedableRandomState::with_seed(NEXT.fetch_add(1, Ordering::Relaxed), secret))
    }
}

impl BuildHasher for Seeded {
    type Hasher = FoldHasher<'static>;

    fn build_hasher(&self) -> Self::Hasher {
        self.0.build_hasher()
    }
}

pub type FastMap<K, V> = std::collections::HashMap<K, V, Seeded>;
pub type FastSet<K> = std::collections::HashSet<K, Seeded>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    #[test]
    fn every_map_hashes_its_own_way_and_each_hashes_alike_throughout() {
        let (a, b) = (Seeded::default(), Seeded::default());
        let key = [7u8; 32];
        assert_eq!(a.hash_one(key), a.hash_one(key));
        assert_ne!(a.hash_one(key), b.hash_one(key), "a fresh seed per map");
        let mut m: FastMap<[u8; 32], u32> = FastMap::default();
        m.insert(key, 1);
        assert_eq!(m.get(&key), Some(&1));
    }
}
