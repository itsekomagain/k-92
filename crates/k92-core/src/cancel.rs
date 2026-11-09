use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// Cooperative cancellation shared between a running job and whoever may cancel it.
#[derive(Clone, Default, Debug)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    /// Set for tokens made with [`CancelToken::child`]: cancelling the parent cancels this too.
    parent: Option<Arc<AtomicBool>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token that is cancelled when either it or `self` is cancelled. Cancelling the child
    /// does not cancel `self` (used to stop helper threads after an internal error).
    pub fn child(&self) -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(self.flag.clone()),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
            || self
                .parent
                .as_ref()
                .is_some_and(|p| p.load(Ordering::SeqCst))
    }

    /// Returns `Err(Cancelled)` once cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_follows_parent_but_not_the_reverse() {
        let parent = CancelToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());

        let child2 = parent.child();
        parent.cancel();
        assert!(child2.is_cancelled());
        assert!(child2.check().is_err());
    }
}
