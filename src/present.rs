//! Decides when the window's event loop wakes. Animation frames follow the display's
//! refresh through a display link where the system has one (macOS 14 and later);
//! elsewhere they stay 16 ms timers.

use std::time::{Duration, Instant};

pub use imp::FrameClock;

/// When the event loop wakes for the page's next timer, due in `ms`: at least 4 ms
/// ahead, as browsers space timer tasks. While the window has been hidden since
/// `hidden`, only on a whole second after that, so a hidden page wakes at most once
/// a second, as browsers throttle the timers of pages nobody sees.
pub fn timer_wake(now: Instant, ms: f64, hidden: Option<Instant>) -> Instant {
    let due = now + Duration::from_secs_f64(ms.max(4.0) / 1e3);
    match hidden {
        None => due,
        Some(since) => since + Duration::from_secs(due.saturating_duration_since(since).as_secs_f64().ceil().max(1.0) as u64),
    }
}

/// Whether the display link runs: only while animation frame callbacks wait and the
/// window can be seen. A hidden page gets no frames, as in browsers.
pub fn frames_run(wanted: bool, hidden: bool) -> bool {
    wanted && !hidden
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes};
    use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
    use std::cell::Cell;
    use std::rc::Rc;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// The view of `window`, which winit makes on the main thread.
    fn view(window: &Window) -> Option<(&AnyObject, MainThreadMarker)> {
        let mtm = MainThreadMarker::new()?;
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
        // SAFETY: `ns_view` is the live NSView of `window`, which `WindowHandle` guarantees
        // for as long as the window lives, and this is the main thread, where views are used.
        Some((unsafe { handle.ns_view.cast().as_ref() }, mtm))
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and `FrameTick` has no `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "RenderFrameTick"]
        #[ivars = Rc<Cell<bool>>]
        struct FrameTick;

        impl FrameTick {
            #[unsafe(method(tick:))]
            fn tick(&self, _link: &CADisplayLink) {
                self.ivars().set(true);
            }
        }

        unsafe impl NSObjectProtocol for FrameTick {}
    );

    /// The display refreshes of the window's screen, while animation frames want them.
    pub struct FrameClock {
        link: Retained<CADisplayLink>,
        refreshed: Rc<Cell<bool>>,
        running: bool,
    }

    impl FrameClock {
        /// A clock for the screen `window` is on; `None` before macOS 14, whose views have no
        /// display link.
        pub fn new(window: &Window) -> Option<FrameClock> {
            let (view, mtm) = view(window)?;
            // SAFETY: `respondsToSelector:` takes a selector and returns BOOL.
            let available: bool = unsafe { msg_send![view, respondsToSelector: sel!(displayLinkWithTarget:selector:)] };
            if !available {
                return None;
            }
            let refreshed = Rc::new(Cell::new(false));
            let target = FrameTick::alloc(mtm).set_ivars(refreshed.clone());
            // SAFETY: `init` is NSObject's initializer.
            let target: Retained<FrameTick> = unsafe { msg_send![super(target), init] };
            // SAFETY: the view has `displayLinkWithTarget:selector:` (checked above), which takes a
            // target and a selector it implements with one CADisplayLink argument, as `tick:` does,
            // and returns a display link that keeps the target alive until it is invalidated.
            let link: Retained<CADisplayLink> = unsafe { msg_send![view, displayLinkWithTarget: &*target, selector: sel!(tick:)] };
            // Animation frames at 60 a second at most, as Safari runs them even on faster displays.
            link.setPreferredFrameRateRange(CAFrameRateRange { minimum: 30.0, maximum: 60.0, preferred: 60.0 });
            link.setPaused(true);
            // SAFETY: `NSRunLoopCommonModes` is an immutable framework constant.
            unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
            Some(FrameClock { link, refreshed, running: false })
        }

        /// True once after the display refreshed while the clock ran.
        pub fn take_refresh(&self) -> bool {
            self.refreshed.replace(false)
        }

        /// Starts or stops the clock; a stopped one wakes nothing.
        pub fn run(&mut self, on: bool) {
            if on != self.running {
                self.running = on;
                self.link.setPaused(!on);
            }
        }
    }

    impl Drop for FrameClock {
        fn drop(&mut self) {
            self.link.invalidate();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use winit::window::Window;

    /// No display link here: animation frames stay 16 ms timers.
    pub struct FrameClock;

    impl FrameClock {
        pub fn new(_window: &Window) -> Option<FrameClock> {
            None
        }

        pub fn take_refresh(&self) -> bool {
            false
        }

        pub fn run(&mut self, _on: bool) {}
    }
}

#[cfg(test)]
mod tests {
    use super::{frames_run, timer_wake};
    use std::time::{Duration, Instant};

    #[test]
    fn a_visible_page_wakes_when_its_timer_is_due_but_at_least_4_ms_ahead() {
        let now = Instant::now();
        assert_eq!(timer_wake(now, 16.0, None), now + Duration::from_millis(16));
        assert_eq!(timer_wake(now, 0.5, None), now + Duration::from_millis(4));
        assert_eq!(timer_wake(now, 2500.0, None), now + Duration::from_millis(2500));
    }

    #[test]
    fn a_hidden_page_wakes_at_most_once_a_second_on_whole_seconds_after_it_was_hidden() {
        let since = Instant::now();
        // A 10 ms interval: every wake-up runs it, and it asks for the next run 10 ms later.
        let mut now = since;
        let wakes: Vec<Duration> = (0..5)
            .map(|_| {
                now = timer_wake(now, 10.0, Some(since));
                now - since
            })
            .collect();
        assert_eq!(wakes, (1..=5).map(Duration::from_secs).collect::<Vec<_>>());
        assert_eq!(timer_wake(since + Duration::from_millis(100), 2500.0, Some(since)), since + Duration::from_secs(3));
        assert_eq!(timer_wake(since + Duration::from_millis(1000), 1000.0, Some(since)), since + Duration::from_secs(2));
    }

    #[test]
    fn animation_frames_run_only_while_they_are_wanted_and_the_window_is_seen() {
        assert!(frames_run(true, false));
        assert!(!frames_run(true, true));
        assert!(!frames_run(false, false));
        assert!(!frames_run(false, true));
    }
}
