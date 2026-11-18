// SPDX-License-Identifier: GPL-3.0-or-later
//! A single-item "latest value wins" mailbox. Producers never block; a
//! consumer that is too slow simply skips stale items.

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

pub struct LatestSlot<T> {
    /// The item, and when the slot last went from empty to full (host monotonic ns): the
    /// oldest moment something was waiting, even if newer items replaced it since.
    item: Mutex<Option<(T, u64)>>,
    cv: Condvar,
}

impl<T> LatestSlot<T> {
    pub fn new() -> Self {
        Self { item: Mutex::new(None), cv: Condvar::new() }
    }

    /// Stores `v`, returning the item it replaced (if it was never taken).
    pub fn put(&self, v: T) -> Option<T> {
        let mut g = self.item.lock().unwrap();
        let since = g.as_ref().map_or_else(crate::proto::now_ns, |(_, t)| *t);
        let old = g.replace((v, since)).map(|(o, _)| o);
        drop(g);
        self.cv.notify_one();
        old
    }

    pub fn take_timeout(&self, timeout: Duration) -> Option<T> {
        self.take_timeout_since(timeout).map(|(v, _)| v)
    }

    /// Like `take_timeout`, also returning since when an item has been waiting.
    pub fn take_timeout_since(&self, timeout: Duration) -> Option<(T, u64)> {
        let guard = self.item.lock().unwrap();
        let (mut guard, _) = self.cv.wait_timeout_while(guard, timeout, |i| i.is_none()).unwrap();
        guard.take()
    }
}
