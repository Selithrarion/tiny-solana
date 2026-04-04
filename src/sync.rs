use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

pub struct TrackedRwLock<T> {
    inner: RwLock<T>,
    name: &'static str,
}

impl<T> TrackedRwLock<T> {
    pub fn new(value: T, name: &'static str) -> Self {
        Self {
            inner: RwLock::new(value),
            name,
        }
    }

    pub fn write(&self) -> TrackedWriteGuard<'_, T> {
        let start = Instant::now();

        let guard = self.inner.write();

        let wait_time = start.elapsed();
        if wait_time > Duration::from_millis(1) {
            eprintln!(
                "[lock:{}] write lock acquired after {:?}",
                self.name, wait_time
            );
        }

        TrackedWriteGuard {
            guard,
            name: self.name,
            acquired_at: Instant::now(),
        }
    }

    pub fn try_write_for(&self, timeout: Duration) -> Option<TrackedWriteGuard<'_, T>> {
        let start = Instant::now();
        let guard = self.inner.try_write_for(timeout)?;
        let wait_time = start.elapsed();
        if wait_time > Duration::from_millis(1) {
            eprintln!(
                "[lock:{}] write lock acquired after {:?}",
                self.name, wait_time
            );
        }
        Some(TrackedWriteGuard {
            guard,
            name: self.name,
            acquired_at: Instant::now(),
        })
    }

    pub fn read(&self) -> TrackedReadGuard<'_, T> {
        let start = Instant::now();
        let guard = self.inner.read();
        let wait_time = start.elapsed();
        if wait_time > Duration::from_millis(1) {
            eprintln!(
                "[lock:{}] read lock acquired after {:?}",
                self.name, wait_time
            );
        }
        TrackedReadGuard {
            guard,
            name: self.name,
            acquired_at: Instant::now(),
        }
    }
}

pub struct TrackedReadGuard<'a, T> {
    guard: RwLockReadGuard<'a, T>,
    name: &'static str,
    acquired_at: Instant,
}

pub struct TrackedWriteGuard<'a, T> {
    guard: RwLockWriteGuard<'a, T>,
    name: &'static str,
    acquired_at: Instant,
}

impl<'a, T> Drop for TrackedReadGuard<'a, T> {
    fn drop(&mut self) {
        let held_time = self.acquired_at.elapsed();
        if held_time > Duration::from_millis(50) {
            eprintln!("[lock:{}] read lock held for {:?}", self.name, held_time);
        }
    }
}

impl<'a, T> Drop for TrackedWriteGuard<'a, T> {
    fn drop(&mut self) {
        let held_time = self.acquired_at.elapsed();
        if held_time > Duration::from_millis(5) {
            eprintln!("[lock:{}] write lock held for {:?}", self.name, held_time);
        }
    }
}

impl<'a, T> Deref for TrackedReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> Deref for TrackedWriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> DerefMut for TrackedWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
