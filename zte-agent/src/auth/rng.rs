//! Entropy for tokens, CSRF secrets and nonces. There is never a fallback to
//! the time or a counter: an entropy failure is an error the caller reports.

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub struct EntropyUnavailable;

pub trait TokenRng: Send + Sync + 'static {
    fn fill(&self, buf: &mut [u8]) -> Result<(), EntropyUnavailable>;
}

/// The kernel CSPRNG (`getrandom(2)`, `/dev/urandom` fallback). It can block
/// until the CRNG is seeded, so it is never called under the auth lock.
pub struct OsRng;

impl TokenRng for OsRng {
    fn fill(&self, buf: &mut [u8]) -> Result<(), EntropyUnavailable> {
        getrandom::getrandom(buf).map_err(|_| EntropyUnavailable)
    }
}

/// Deterministic bytes for tests: each call is distinct (the call number
/// leads the buffer), so a table full of test tokens never collides.
#[cfg(test)]
pub struct SeqRng(AtomicU64);

#[cfg(test)]
impl SeqRng {
    pub fn new() -> Self {
        SeqRng(AtomicU64::new(1))
    }
}

#[cfg(test)]
impl TokenRng for SeqRng {
    fn fill(&self, buf: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        let lead = n.to_be_bytes();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = lead.get(i).copied().unwrap_or((n as u8).wrapping_add(i as u8));
        }
        Ok(())
    }
}

#[cfg(test)]
pub struct FailRng;

#[cfg(test)]
impl TokenRng for FailRng {
    fn fill(&self, _buf: &mut [u8]) -> Result<(), EntropyUnavailable> {
        Err(EntropyUnavailable)
    }
}
