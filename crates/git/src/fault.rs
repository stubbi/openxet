//! Test hooks at the points where the pack lifecycle races other writers
//! (ADR 0004 item 5), so a test can run a push or touch the cache exactly
//! there. Built only with the test-only `fault-injection` feature.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::Engine;

/// Where a [`FaultHook`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    /// In a repack, after the working copy's snapshot is built and before
    /// its packs' mtimes are pinned again and git runs; the hook gets an
    /// empty string.
    RepackBeforeGit,
    /// In the pack sweep, after it read the pack rows and before it claims
    /// any deletion; the hook gets an empty string.
    SweepAfterRead,
    /// In the pack sweep, after it claimed the deletion of a pack's files
    /// and before they go; the hook gets the pack's name.
    SweepAfterClaim,
}

/// A hook: called with the point's argument, it returns the work to await
/// there.
pub type FaultHook = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Default)]
pub(crate) struct Hooks(Mutex<HashMap<FaultPoint, FaultHook>>);

impl Engine {
    /// Installs `hook` at `point`, replacing any there; `None` removes it.
    pub fn set_fault_hook(&self, point: FaultPoint, hook: Option<FaultHook>) {
        let mut hooks = self
            .hooks
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match hook {
            Some(h) => hooks.insert(point, h),
            None => hooks.remove(&point),
        };
    }

    /// Runs the hook at `point`, if any, with `arg`.
    pub(crate) async fn fault(&self, point: FaultPoint, arg: &str) {
        let hook = self
            .hooks
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&point)
            .cloned();
        if let Some(hook) = hook {
            hook(arg.to_owned()).await;
        }
    }
}
