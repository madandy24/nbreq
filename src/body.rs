use std::sync::Arc;

/// Immutable, shared storage for a buffered [`Response`](crate::Response) body.
///
/// Cloning shares the byte allocation. Use [`Self::as_bytes`] to borrow it, or
/// [`Self::try_into_vec`] to take its original allocation when no other owner remains.
/// A body can outlive its Engine without keeping network workers or sockets alive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseBody {
    bytes: Arc<crate::body_budget::BodyBuffer>,
}

impl ResponseBody {
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self::from_buffer(crate::body_budget::BodyBuffer::from_vec(bytes))
    }

    pub(crate) fn from_buffer(bytes: crate::body_budget::BodyBuffer) -> Self {
        Self {
            bytes: Arc::new(bytes),
        }
    }

    /// Borrows the body bytes without copying them.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// Transfers uniquely owned storage into an application-owned vector without copying.
    ///
    /// Preserves the vector's allocation, length and capacity. If another response or body
    /// shares this allocation, returns this owner unchanged in `Err`; it never silently copies.
    /// Drop the other owners and retry, or explicitly copy [`Self::as_bytes`] if needed.
    ///
    /// The returned vector belongs to the application; consumers must budget its storage and
    /// any subsequent decoded data. Taking ownership does not free that memory.
    pub fn try_into_vec(self) -> Result<Vec<u8>, Self> {
        Arc::try_unwrap(self.bytes)
            .map(crate::body_budget::BodyBuffer::into_vec)
            .map_err(|bytes| Self { bytes })
    }
}

impl AsRef<[u8]> for ResponseBody {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
