// SPDX-License-Identifier: GPL-3.0-or-later
//! A single-item "latest value wins" mailbox. Producers never block; a
//! consumer that is too slow simply skips stale items.

use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

pub struct LatestSlot<T> {
    item: Mutex<Option<T>>,
    cv: Condvar,
}

impl<T> LatestSlot<T> {
    pub fn new() -> Self {
        Self { item: Mutex::new(None), cv: Condvar::new() }
    }

    /// Stores `v`, returning the item it replaced (if it was never taken).
    pub fn put(&self, v: T) -> Option<T> {
        let old = self.item.lock().unwrap().replace(v);
        self.cv.notify_one();
        old
    }

    pub fn take_timeout(&self, timeout: Duration) -> Option<T> {
        let guard = self.item.lock().unwrap();
        let (mut guard, _) = self.cv.wait_timeout_while(guard, timeout, |i| i.is_none()).unwrap();
        guard.take()
    }
}
