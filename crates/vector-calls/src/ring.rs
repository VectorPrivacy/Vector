//! A single-producer, single-consumer sample ring the audio callbacks can touch:
//! no lock, no allocation, a dropped sample on overflow rather than a stall.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

pub struct SpscRing {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// Next slot to write; only the producer advances it.
    tail: AtomicUsize,
    /// Next slot to read; only the consumer advances it.
    head: AtomicUsize,
}

impl SpscRing {
    /// Capacity rounds up to a power of two.
    pub fn new(min_capacity: usize) -> Self {
        let cap = min_capacity.next_power_of_two().max(2);
        let buf = (0..cap).map(|_| AtomicU32::new(0)).collect::<Vec<_>>().into_boxed_slice();
        Self { buf, mask: cap - 1, tail: AtomicUsize::new(0), head: AtomicUsize::new(0) }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Samples waiting to be read.
    pub fn len(&self) -> usize {
        self.tail.load(Ordering::Acquire).wrapping_sub(self.head.load(Ordering::Acquire))
    }

    /// Writes what fits and returns how many samples were taken.
    pub fn push(&self, samples: &[f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let free = self.capacity() - tail.wrapping_sub(head);
        let n = samples.len().min(free);
        for (i, s) in samples[..n].iter().enumerate() {
            self.buf[(tail.wrapping_add(i)) & self.mask].store(s.to_bits(), Ordering::Relaxed);
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// Reads up to `out.len()` samples and returns how many were filled.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let n = out.len().min(tail.wrapping_sub(head));
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(self.buf[(head.wrapping_add(i)) & self.mask].load(Ordering::Relaxed));
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer side: throws away the oldest `n` samples.
    pub fn skip(&self, n: usize) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let n = n.min(tail.wrapping_sub(head));
        self.head.store(head.wrapping_add(n), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_come_out_in_order_and_overflow_drops_the_newest() {
        let ring = SpscRing::new(8);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0]), 3);
        let mut out = [0.0; 2];
        assert_eq!(ring.pop(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        let ten: Vec<f32> = (0..10).map(|i| i as f32).collect();
        assert_eq!(ring.push(&ten), 7);
        assert_eq!(ring.len(), 8);
        ring.skip(1);
        let mut rest = [0.0; 8];
        assert_eq!(ring.pop(&mut rest), 7);
        assert_eq!(&rest[..7], &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }
}
