#![cfg_attr(not(feature = "native"), allow(dead_code))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Error, LimitKind};

/// Only accounting state: owners may retain this after the Engine and all workers have stopped.
#[derive(Debug)]
pub(crate) struct BodyBudget {
    limit: Option<usize>,
    used: AtomicUsize,
    peak: AtomicUsize,
}

impl BodyBudget {
    pub(crate) fn new(limit: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    pub(crate) fn acquire(self: &Arc<Self>, bytes: usize) -> Result<BodyPermit, Error> {
        if bytes == 0 {
            return Ok(BodyPermit {
                budget: Arc::clone(self),
                bytes,
            });
        }
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let next = used
                .checked_add(bytes)
                .filter(|next| self.limit.is_none_or(|limit| *next <= limit))
                .ok_or_else(|| {
                    Error::limit(
                        LimitKind::BufferedBodyBytes,
                        "aggregate buffered HTTP body capacity exhausted",
                    )
                })?;
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    return Ok(BodyPermit {
                        budget: Arc::clone(self),
                        bytes,
                    });
                }
                Err(current) => used = current,
            }
        }
    }

    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    pub(crate) fn is_limited(&self) -> bool {
        self.limit.is_some()
    }
    pub(crate) fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub(crate) struct BodyPermit {
    budget: Arc<BodyBudget>,
    bytes: usize,
}

impl Drop for BodyPermit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// An allocation and its charge. Declaration order is significant: free bytes before refund.
#[derive(Debug, Default)]
pub(crate) struct BodyBuffer {
    bytes: Vec<u8>,
    permit: Option<BodyPermit>,
    budget: Option<Arc<BodyBudget>>,
    future: Option<BodyPermit>,
    planned_capacity: usize,
}

impl BodyBuffer {
    pub(crate) fn new(budget: Option<Arc<BodyBudget>>) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            ..Self::default()
        }
    }
    pub(crate) fn admit(bytes: Vec<u8>, budget: Arc<BodyBudget>) -> Result<Self, Error> {
        let permit = budget.acquire(bytes.capacity())?;
        Ok(Self {
            bytes,
            permit: Some(permit),
            budget: Some(budget),
            ..Self::default()
        })
    }
    pub(crate) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn budget(&self) -> Option<Arc<BodyBudget>> {
        self.budget.clone()
    }

    pub(crate) fn plan_capacity(&mut self, capacity: usize) -> Result<(), Error> {
        check_capacity(capacity)?;
        // Framing reserves permission; a peer that sends no payload need not cause allocation.
        if let Some(budget) = &self.budget {
            self.future = Some(budget.acquire(capacity)?);
            self.planned_capacity = capacity;
        }
        Ok(())
    }

    /// Reserve the replacement while the old allocation is still charged. Use a fixed-size
    /// boxed slice as the allocation boundary: its entire length is charged, then conversion
    /// to Vec preserves that length as capacity. Allocator bookkeeping/rounding is headroom.
    pub(crate) fn reserve_capacity(&mut self, capacity: usize) -> Result<(), Error> {
        let capacity = capacity.max(self.planned_capacity);
        check_capacity(capacity)?;
        if capacity <= self.bytes.capacity() {
            return Ok(());
        }
        let permit = if capacity == self.planned_capacity && self.future.is_some() {
            self.future.take()
        } else {
            self.budget
                .as_ref()
                .map(|budget| budget.acquire(capacity))
                .transpose()?
        };
        self.planned_capacity = 0;
        let mut bytes = vec![0; capacity].into_boxed_slice().into_vec();
        bytes[..self.bytes.len()].copy_from_slice(&self.bytes);
        bytes.truncate(self.bytes.len());
        let old_bytes = std::mem::replace(&mut self.bytes, bytes);
        drop(old_bytes);
        self.permit = permit;
        Ok(())
    }

    pub(crate) fn extend(&mut self, bytes: &[u8], ceiling: usize) -> Result<(), Error> {
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= ceiling)
            .ok_or_else(|| {
                Error::limit(
                    LimitKind::BufferedBodyBytes,
                    "buffered HTTP staging capacity exceeded",
                )
            })?;
        if self.budget.is_none() {
            self.bytes.extend_from_slice(bytes);
            return Ok(());
        }
        if needed > self.capacity() {
            let capacity = if self.planned_capacity != 0 {
                self.planned_capacity.max(needed)
            } else {
                needed
                    .max(self.capacity().saturating_mul(2))
                    .max(4096)
                    .min(ceiling)
            };
            self.reserve_capacity(capacity)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    #[inline]
    pub(crate) fn push(&mut self, byte: u8, ceiling: usize) -> Result<(), Error> {
        if self.bytes.len() < self.capacity() || self.budget.is_none() {
            self.bytes.push(byte);
            Ok(())
        } else {
            self.extend(&[byte], ceiling)
        }
    }

    /// Explicit application ownership transfer is the one exception to free-before-refund.
    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::ops::Deref for BodyBuffer {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl AsRef<[u8]> for BodyBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl<T: AsRef<[u8]>> PartialEq<T> for BodyBuffer {
    fn eq(&self, other: &T) -> bool {
        self.bytes == other.as_ref()
    }
}
impl Eq for BodyBuffer {}

fn check_capacity(capacity: usize) -> Result<(), Error> {
    if capacity > isize::MAX as usize {
        Err(Error::limit(
            LimitKind::BufferedBodyBytes,
            "buffered HTTP capacity exceeds the addressable allocation size",
        ))
    } else {
        Ok(())
    }
}

impl From<Vec<u8>> for BodyBuffer {
    fn from(bytes: Vec<u8>) -> Self {
        Self::from_vec(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3_growth_charges_old_and_new_capacity_and_failure_preserves_original() {
        let budget = BodyBudget::new(Some(12 * 1024));
        let mut body = BodyBuffer::new(Some(budget.clone()));
        body.extend(&vec![1; 4096], usize::MAX)
            .expect("initial allocation");
        let original = body.as_ptr();
        let competing = budget.acquire(1).expect("one competing byte");
        let error = body
            .push(2, usize::MAX)
            .expect_err("old plus new exceeds budget");
        assert_eq!(error.limit_kind(), Some(LimitKind::BufferedBodyBytes));
        assert_eq!(body.as_ptr(), original);
        assert_eq!(body.len(), 4096);
        assert_eq!(budget.used(), 4097);
        drop(competing);
        body.push(2, usize::MAX)
            .expect("growth fits after competing release");
        assert_eq!(body.len(), 4097);
        assert_eq!(body.capacity(), 8192);
        assert_eq!(budget.peak(), 12 * 1024);
        assert_eq!(budget.used(), 8192);
        drop(body);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn m3_known_length_reserves_without_allocating_and_unused_reservation_refunds() {
        let budget = BodyBudget::new(Some(8192));
        let mut body = BodyBuffer::new(Some(budget.clone()));
        body.plan_capacity(8192).expect("reserve known reply");
        assert_eq!(body.capacity(), 0);
        assert_eq!(budget.used(), 8192);
        drop(body);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn m3_concurrent_admission_never_exceeds_capacity() {
        let budget = BodyBudget::new(Some(4 * 4096));
        let entered = Arc::new(std::sync::Barrier::new(17));
        let released = Arc::new(std::sync::Barrier::new(17));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (budget, entered, released) =
                    (budget.clone(), entered.clone(), released.clone());
                std::thread::spawn(move || {
                    let body = BodyBuffer::admit(vec![0; 4096], budget);
                    entered.wait();
                    released.wait();
                    body.is_ok()
                })
            })
            .collect();
        entered.wait();
        assert_eq!(budget.used(), 4 * 4096);
        released.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| usize::from(thread.join().expect("thread")))
                .sum::<usize>(),
            4
        );
        assert_eq!(budget.peak(), 4 * 4096);
        assert_eq!(budget.used(), 0);
    }
}
