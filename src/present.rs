//! Puts painted frames in the window, and decides when the window's event loop wakes.
//!
//! On macOS a frame is painted straight into one of a few IOSurfaces, and that
//! surface becomes the contents of a Core Animation layer, which the window server
//! composites without copying it. softbuffer, used elsewhere, allocates and zeroes
//! a new buffer for every frame and hands Core Animation a CGImage to copy.
//! Animation frames follow the display's refresh through a display link where the
//! system has one (macOS 14 and later); elsewhere they stay 16 ms timers.

use std::time::{Duration, Instant};

pub use crate::paint::Rect;
pub use imp::{Alarm, FrameClock, Presenter};

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
    use objc2_core_foundation::{
        kCFRunLoopCommonModes, CFAbsoluteTimeGetCurrent, CFDictionary, CFNumber, CFNumberType, CFRetained, CFRunLoop, CFRunLoopTimer, CFString, CFType,
        CGPoint, CGRect,
    };
    use objc2_core_graphics::{kCGColorSpaceSRGB, CGColorSpace};
    use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes};
    use objc2_io_surface::{
        kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfaceColorSpace, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
        IOSurfaceLockOptions, IOSurfaceRef,
    };
    use objc2_quartz_core::{kCAGravityTopLeft, CADisplayLink, CAFrameRateRange, CALayer, CATransaction};
    use super::Rect;
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;
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

    /// Set by the alarm's timer when it fires, until `Alarm::take_fired`.
    static FIRED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C-unwind" fn ring(_timer: *mut CFRunLoopTimer, _info: *mut c_void) {
        FIRED.store(true, Ordering::Relaxed);
    }

    /// Wakes the event loop at a set time, through a run loop timer of its own. winit's
    /// `ControlFlow::WaitUntil` arms a timer that repeats every 0.1 us, and when the system
    /// fires it late, as it does for a process that idles, Core Foundation steps its next
    /// fire date past the present one interval at a time: millions of instructions for each
    /// wake-up. This timer repeats once in 30 years, which no wait reaches.
    pub struct Alarm {
        timer: CFRetained<CFRunLoopTimer>,
        at: Option<Instant>,
    }

    impl Alarm {
        pub fn new() -> Option<Alarm> {
            // SAFETY: `ring` has the callout signature and reads no context, so none is passed.
            let timer = unsafe { CFRunLoopTimer::new(None, f64::MAX, 1e9, 0, 0, Some(ring), std::ptr::null_mut()) }?;
            // SAFETY: `kCFRunLoopCommonModes` is an immutable framework constant.
            CFRunLoop::main()?.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
            Some(Alarm { timer, at: None })
        }

        /// Rings at `at`, at once when that has passed, or never for `None`.
        pub fn set(&mut self, at: Option<Instant>) {
            if at == self.at {
                return;
            }
            self.at = at;
            let fire = at.map_or(f64::MAX, |at| CFAbsoluteTimeGetCurrent() + at.saturating_duration_since(Instant::now()).as_secs_f64());
            self.timer.set_next_fire_date(fire);
        }

        /// True once after the alarm rang.
        pub fn take_fired(&mut self) -> bool {
            let fired = FIRED.swap(false, Ordering::Relaxed);
            if fired {
                self.at = None;
            }
            fired
        }
    }

    impl Drop for Alarm {
        fn drop(&mut self) {
            self.timer.invalidate();
        }
    }

    /// `kCVPixelFormatType_32BGRA`: a 0xAARRGGBB `u32` in little-endian memory.
    const BGRA: i32 = i32::from_be_bytes(*b"BGRA");

    /// Most surfaces kept: the one on screen, the one being painted, and two for
    /// frames the window server has not let go of yet. A new one is made only when
    /// every other is still in use.
    const POOL: usize = 4;

    pub struct Presenter {
        /// The view's own layer, whose bounds and scale ours follows.
        root: Retained<CALayer>,
        layer: Retained<CALayer>,
        surfaces: Vec<CFRetained<IOSurfaceRef>>,
        /// For each surface, the number of the frame last presented from it.
        shown: Vec<u64>,
        /// The surface on screen.
        front: Option<usize>,
        frames: u64,
        w: usize,
        h: usize,
        bounds: CGRect,
        scale: f64,
        /// Painted into instead of the surface when the surface pads its rows.
        scratch: Vec<u32>,
    }

    /// A surface locked for painting: `pixels` are its `w * h` pixels, 0x00RRGGBB.
    pub struct Frame<'a> {
        p: &'a mut Presenter,
        index: usize,
        locked: bool,
    }

    fn number(v: i32) -> CFRetained<CFNumber> {
        // SAFETY: `value_ptr` points to a live i32, matching `SInt32Type`; no allocator means the default one.
        unsafe { CFNumber::new(None, CFNumberType::SInt32Type, (&v as *const i32).cast()) }.expect("create CFNumber")
    }

    impl Presenter {
        pub fn new(window: &Rc<Window>) -> Result<Presenter, String> {
            let (view, _) = view(window).ok_or("not an AppKit window on the main thread")?;
            // SAFETY: `setWantsLayer:` takes a BOOL and `layer` returns a nullable CALayer, as
            // declared here.
            let root: Retained<CALayer> = unsafe {
                let _: () = msg_send![view, setWantsLayer: true];
                let layer: Option<Retained<CALayer>> = msg_send![view, layer];
                layer.ok_or("the view did not become layer-backed")?
            };
            // The same geometry as softbuffer's layer: top-left origin, contents at the top left.
            let layer = CALayer::new();
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            layer.setGeometryFlipped(true);
            // SAFETY: `kCAGravityTopLeft` is an immutable framework constant.
            layer.setContentsGravity(unsafe { kCAGravityTopLeft });
            layer.setOpaque(true);
            root.addSublayer(&layer);
            CATransaction::commit();
            let bounds = layer.frame();
            Ok(Presenter { root, layer, surfaces: Vec::new(), shown: Vec::new(), front: None, frames: 0, w: 0, h: 0, bounds, scale: 0.0, scratch: Vec::new() })
        }

        fn surface(&self) -> CFRetained<IOSurfaceRef> {
            // SAFETY: the `kIOSurface*` keys are immutable framework constants.
            let keys: [&CFString; 5] = unsafe { [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfacePixelFormat] };
            let values = [number(self.w as i32), number(self.h as i32), number(4), number(self.w as i32 * 4), number(BGRA)];
            let values: [&CFNumber; 5] = [&values[0], &values[1], &values[2], &values[3], &values[4]];
            let props = CFDictionary::from_slices(&keys, &values);
            // SAFETY: `props` maps CFString keys to CFNumber values, the types IOSurfaceCreate takes.
            let surface = unsafe { IOSurfaceRef::new(props.as_opaque()) }.expect("create IOSurface");
            // Tagged sRGB, Core Animation matches the pixels to the display exactly as it
            // matches the DeviceRGB images softbuffer presents.
            // SAFETY: `kCGColorSpaceSRGB` is an immutable framework constant.
            if let Some(list) = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).and_then(|s| s.property_list()) {
                // SAFETY: the value of `kIOSurfaceColorSpace` is a colour space property list, which this is.
                unsafe { surface.set_value(kIOSurfaceColorSpace, &list) };
            }
            surface
        }

        /// Matches the surfaces to `w` x `h` device pixels, and the layer to the view at `scale`.
        pub fn resize(&mut self, w: u32, h: u32, scale: f64) {
            let bounds = self.root.bounds();
            if bounds != self.bounds || scale != self.scale {
                CATransaction::begin();
                CATransaction::setDisableActions(true);
                self.layer.setFrame(bounds);
                self.layer.setContentsScale(scale);
                CATransaction::commit();
                (self.bounds, self.scale) = (bounds, scale);
            }
            let (w, h) = (w as usize, h as usize);
            if (w, h) != (self.w, self.h) {
                // The layer keeps the surface it shows until the next frame replaces it.
                self.surfaces.clear();
                self.shown.clear();
                self.front = None;
                self.scratch = Vec::new();
                (self.w, self.h) = (w, h);
            }
        }

        /// How many frames have been presented; the next one gets the number after it.
        pub fn shown(&self) -> u64 {
            self.frames
        }

        /// Keeps only the surface on screen, for a window nobody sees.
        pub fn release_spares(&mut self) {
            let Some(front) = self.front else { return };
            self.surfaces.swap(0, front);
            self.shown.swap(0, front);
            self.surfaces.truncate(1);
            self.shown.truncate(1);
            self.front = Some(0);
        }

        /// Locks a surface the window server is not reading for painting the next frame.
        pub fn frame(&mut self) -> Result<Frame<'_>, String> {
            let free = (0..self.surfaces.len()).find(|&i| Some(i) != self.front && !self.surfaces[i].is_in_use());
            let index = match free {
                Some(i) => i,
                None if self.surfaces.len() < POOL => {
                    let surface = self.surface();
                    if surface.bytes_per_row() != self.w * 4 && self.scratch.len() != self.w * self.h {
                        self.scratch = vec![0; self.w * self.h];
                    }
                    self.surfaces.push(surface);
                    self.shown.push(0);
                    self.surfaces.len() - 1
                }
                // All in use: the one presented longest ago.
                None => (0..self.surfaces.len()).filter(|&i| Some(i) != self.front).min_by_key(|&i| self.shown[i]).unwrap_or(0),
            };
            // SAFETY: a null seed pointer is allowed; the surface is a live IOSurface.
            let rc = unsafe { self.surfaces[index].lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
            if rc != 0 {
                return Err(format!("lock the frame surface: error {rc}"));
            }
            Ok(Frame { p: self, index, locked: true })
        }
    }

    impl Drop for Presenter {
        fn drop(&mut self) {
            self.layer.removeFromSuperlayer();
        }
    }

    impl Frame<'_> {
        fn stride(&self) -> usize {
            self.p.surfaces[self.index].bytes_per_row()
        }

        /// The number of the frame whose pixels the surface still holds, or 0 when it
        /// holds none worth keeping and the frame must be painted whole.
        pub fn held(&self) -> u64 {
            if self.stride() == self.p.w * 4 { self.p.shown[self.index] } else { 0 }
        }

        pub fn pixels(&mut self) -> &mut [u32] {
            let (w, h) = (self.p.w, self.p.h);
            if self.stride() != w * 4 {
                return &mut self.p.scratch;
            }
            let base = self.p.surfaces[self.index].base_address();
            // SAFETY: the surface is locked for writing while this frame lives, so its base address
            // is valid, page-aligned memory of `h * stride` bytes, and the stride is `w * 4` here, so
            // it holds exactly `w * h` u32s. The `&mut self` borrow makes this the only reference to it.
            unsafe { std::slice::from_raw_parts_mut(base.as_ptr().cast::<u32>(), w * h) }
        }

        fn unlock(&mut self) -> Result<(), String> {
            self.locked = false;
            // SAFETY: balances the lock taken in `Presenter::frame`, with the same options.
            let rc = unsafe { self.p.surfaces[self.index].unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
            if rc == 0 { Ok(()) } else { Err(format!("unlock the frame surface: error {rc}")) }
        }

        /// Shows the frame, of which `painted` (all of it for `None`) was painted anew.
        /// Those pixels get an opaque alpha first: Core Animation blends IOSurface contents
        /// by their alpha even in an opaque layer.
        pub fn present(mut self, painted: Option<&[Rect]>) -> Result<(), String> {
            let (w, stride) = (self.p.w, self.stride());
            if let (Some(parts), true) = (painted, stride == w * 4) {
                let px = self.pixels();
                for r in parts {
                    for y in r.y0..r.y1 {
                        for p in &mut px[y as usize * w + r.x0 as usize..y as usize * w + r.x1 as usize] {
                            *p |= 0xFF00_0000;
                        }
                    }
                }
            } else if stride != w * 4 {
                let base = self.p.surfaces[self.index].base_address().as_ptr().cast::<u8>();
                for (y, row) in self.p.scratch.chunks_exact(w).enumerate() {
                    // SAFETY: the surface is locked for writing, so its base address is valid for
                    // `h * stride` bytes; row `y < h` starts at `y * stride` and `w * 4 <= stride`
                    // bytes fit in it. The scratch buffer is our own memory, not the surface's.
                    let dst = unsafe { std::slice::from_raw_parts_mut(base.add(y * stride).cast::<u32>(), w) };
                    for (d, s) in dst.iter_mut().zip(row) {
                        *d = s | 0xFF00_0000;
                    }
                }
            } else {
                for p in self.pixels() {
                    *p |= 0xFF00_0000;
                }
            }
            self.unlock()?;
            let p = &mut *self.p;
            p.frames += 1;
            p.shown[self.index] = p.frames;
            p.front = Some(self.index);
            let contents: &AnyObject = AsRef::<CFType>::as_ref(&*p.surfaces[self.index]).as_ref();
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            // SAFETY: an IOSurface is a documented `contents` type for a CALayer on macOS.
            unsafe { p.layer.setContents(Some(contents)) };
            CATransaction::commit();
            Ok(())
        }
    }

    impl Drop for Frame<'_> {
        fn drop(&mut self) {
            if self.locked {
                // A frame that is not presented still releases its lock; nothing waits on the result.
                let _ = self.unlock();
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::num::NonZeroU32;
    use std::rc::Rc;
    use winit::window::Window;

    /// No run loop timer here: the event loop waits with `ControlFlow::WaitUntil`.
    pub struct Alarm;

    impl Alarm {
        pub fn new() -> Option<Alarm> {
            None
        }

        pub fn set(&mut self, _at: Option<std::time::Instant>) {}

        pub fn take_fired(&mut self) -> bool {
            false
        }
    }

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

    pub struct Presenter {
        surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
        size: (NonZeroU32, NonZeroU32),
    }

    /// A frame buffer from softbuffer: `pixels` are its `w * h` pixels, 0x00RRGGBB.
    pub struct Frame<'a> {
        buffer: softbuffer::Buffer<'a, Rc<Window>, Rc<Window>>,
    }

    impl Presenter {
        pub fn new(window: &Rc<Window>) -> Result<Presenter, String> {
            let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
            let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
            Ok(Presenter { surface, size: (NonZeroU32::MIN, NonZeroU32::MIN) })
        }

        pub fn resize(&mut self, w: u32, h: u32, _scale: f64) {
            if let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
                self.size = (w, h);
            }
        }

        pub fn release_spares(&mut self) {}

        pub fn shown(&self) -> u64 {
            0
        }

        pub fn frame(&mut self) -> Result<Frame<'_>, String> {
            self.surface.resize(self.size.0, self.size.1).map_err(|e| e.to_string())?;
            let buffer = self.surface.buffer_mut().map_err(|e| e.to_string())?;
            Ok(Frame { buffer })
        }
    }

    impl Frame<'_> {
        pub fn pixels(&mut self) -> &mut [u32] {
            &mut self.buffer
        }

        /// softbuffer gives a fresh buffer every frame.
        pub fn held(&self) -> u64 {
            0
        }

        pub fn present(self, _painted: Option<&[super::Rect]>) -> Result<(), String> {
            self.buffer.present().map_err(|e| e.to_string())
        }
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
