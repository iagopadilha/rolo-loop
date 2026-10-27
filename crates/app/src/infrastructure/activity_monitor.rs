// SPDX-License-Identifier: MIT
//! User activity and app busyness, for the idle shutdown.
//!
//! The app only shuts down on its own when NO busy provider is busy (loop running, chat/roadmap in progress, relaunched loop alive) AND there was
//! no activity for `idle_hours`. `idle_hours == 0` = never shuts down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Something that can keep the app awake.
pub trait BusyProvider: Send + Sync {
    /// True while there is work that prevents the shutdown (e.g. a loop running in some open project).
    fn is_busy(&self) -> bool;
}

pub struct ActivityMonitor {
    idle_seconds: f64,
    on_idle: Box<dyn Fn() + Send + Sync>,
    busy_providers: Mutex<Vec<Arc<dyn BusyProvider>>>,
    last_activity: Mutex<Instant>,
    stop: Arc<AtomicBool>,
}

impl ActivityMonitor {
    pub fn new(idle_hours: f64, on_idle: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(ActivityMonitor {
            idle_seconds: idle_hours * 3600.0,
            on_idle: Box::new(on_idle),
            busy_providers: Mutex::new(Vec::new()),
            last_activity: Mutex::new(Instant::now()),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn add_busy_provider(&self, provider: Arc<dyn BusyProvider>) {
        if let Ok(mut list) = self.busy_providers.lock() {
            list.push(provider);
        }
    }

    pub fn record_activity(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    pub fn is_any_provider_busy(&self) -> bool {
        let providers: Vec<Arc<dyn BusyProvider>> = self.busy_providers.lock().map(|l| l.clone()).unwrap_or_default();
        providers.iter().any(|p| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.is_busy())).unwrap_or(true))
        // when in doubt, do not shut down
    }

    fn idle_for(&self) -> f64 {
        self.last_activity.lock().map(|l| l.elapsed().as_secs_f64()).unwrap_or(0.0)
    }

    /// Seconds until the idle shutdown, or `None` when it cannot happen (disabled, or something is busy).
    pub fn seconds_until_shutdown(&self) -> Option<i64> {
        if self.idle_seconds <= 0.0 || self.is_any_provider_busy() {
            return None;
        }
        Some((self.idle_seconds - self.idle_for()).max(0.0) as i64)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Watches in a background thread; `None` when the shutdown is disabled.
    pub fn watch_idle_in_background(self: &Arc<Self>) -> Option<std::thread::JoinHandle<()>> {
        if self.idle_seconds <= 0.0 {
            return None;
        }
        let monitor = Arc::clone(self);
        Some(std::thread::spawn(move || {
            while !monitor.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if monitor.is_any_provider_busy() {
                    monitor.record_activity();
                } else if monitor.idle_for() >= monitor.idle_seconds {
                    (monitor.on_idle)();
                    return;
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Flag(AtomicBool);
    impl BusyProvider for Flag {
        fn is_busy(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn idle_shutdown_waits_for_busy_providers() {
        let fired = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fired);
        let monitor = ActivityMonitor::new(0.0, || {});
        assert!(monitor.seconds_until_shutdown().is_none() && monitor.watch_idle_in_background().is_none());
        let counter2 = Arc::clone(&counter);
        let monitor = ActivityMonitor::new(1.0 / 3600.0 * 1.2, move || {
            counter2.fetch_add(1, Ordering::Relaxed);
        });
        let busy = Arc::new(Flag(AtomicBool::new(true)));
        monitor.add_busy_provider(busy.clone());
        assert!(monitor.is_any_provider_busy() && monitor.seconds_until_shutdown().is_none());
        let watcher = monitor.watch_idle_in_background().unwrap();
        std::thread::sleep(Duration::from_millis(1800));
        assert_eq!(fired.load(Ordering::Relaxed), 0);
        busy.0.store(false, Ordering::Relaxed);
        assert!(monitor.seconds_until_shutdown().is_some());
        watcher.join().unwrap();
        assert_eq!(fired.load(Ordering::Relaxed), 1);
    }
}
