use std::fmt;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

/// A wrapper that zeroizes its contents on drop
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, ZeroizeOnDrop)]
pub struct Secret<T: zeroize::Zeroize>(#[zeroize(drop)] T);

impl<T> Secret<T>
where
    T: zeroize::Zeroize,
{
    /// Create a new secret wrapper
    pub fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> AsMut<T> for Secret<T>
where
    T: zeroize::Zeroize,
{
    fn as_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> AsRef<T> for Secret<T>
where
    T: zeroize::Zeroize,
{
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T> Deref for Secret<T>
where
    T: zeroize::Zeroize,
{
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Secret<T>
where
    T: zeroize::Zeroize,
{
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> fmt::Debug for Secret<T>
where
    T: zeroize::Zeroize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Don't leak secret in debug output
        write!(f, "Secret(***)")
    }
}

// Serialization support
impl<T> Serialize for Secret<T>
where
    T: zeroize::Zeroize + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for Secret<T>
where
    T: zeroize::Zeroize + Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Secret)
    }
}

// Re-export Zeroize trait from zeroize crate for convenience
pub use zeroize::Zeroize;
