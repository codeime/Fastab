//! CVDisplayLinkStop does not join its IO thread. Releasing the link (or the
//! callback's context) immediately afterwards caused native crashes upstream.
//! Keep one clock per encountered display for the process lifetime, and remove
//! each window's source under the callback's lock before cancelling/releasing it.
//! See https://github.com/zed-industries/zed/pull/32116 and
//! https://github.com/zed-industries/zed/pull/60696.
use crate::{
    dispatch_get_main_queue,
    dispatch_sys::{
        _dispatch_source_type_data_add, dispatch_object_t, dispatch_queue_t, dispatch_resume,
        dispatch_set_context, dispatch_source_cancel, dispatch_source_create,
        dispatch_source_merge_data, dispatch_source_set_event_handler_f, dispatch_source_t,
    },
};
use anyhow::Result;
use core_graphics::display::CGDirectDisplayID;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};
use util::ResultExt;

// These functions are not in this fork's generated dispatch bindings.
unsafe extern "C" {
    fn dispatch_release(object: dispatch_object_t);
    fn dispatch_set_finalizer_f(
        object: dispatch_object_t,
        finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
    );
}

static REGISTRY: Registry<CoreVideoClock> = Registry::new();

/// Mutations are serialized on the main thread. The CV callback only reads the
/// map and merges events, holding this lock until it has finished using sources.
/// Never call CoreVideo while holding the lock: its IO thread may hold internal
/// locks when it calls us, so doing so could deadlock with start/stop.
struct Registry<C> {
    displays: Mutex<BTreeMap<CGDirectDisplayID, DisplayEntry<C>>>,
}

struct DisplayEntry<C> {
    clock: Arc<C>,
    running: bool,
    subscribers: Vec<Arc<FrameRequests>>,
}

trait Clock {
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
}

struct CoreVideoClock(sys::DisplayLink);

// Only the main thread creates/starts/stops clocks. The callback does not touch
// this handle, and the static registry keeps its owning reference forever.
unsafe impl Send for CoreVideoClock {}
unsafe impl Sync for CoreVideoClock {}

impl Clock for CoreVideoClock {
    fn start(&self) -> Result<()> {
        unsafe { self.0.start() }
    }

    fn stop(&self) -> Result<()> {
        unsafe { self.0.stop() }
    }
}

impl<C: Clock> Registry<C> {
    const fn new() -> Self {
        Self {
            displays: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<CGDirectDisplayID, DisplayEntry<C>>> {
        // Never unwind through the CV callback. Partial main-thread mutations
        // leave valid entries and owned source handles even after a panic.
        self.displays.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn subscribe(
        &self,
        display_id: CGDirectDisplayID,
        source: &Arc<FrameRequests>,
        create_clock: impl FnOnce() -> Result<C>,
    ) -> Result<()> {
        if !self.lock().contains_key(&display_id) {
            // Setup errors release a never-started clock safely. Once inserted,
            // an entry is never removed, even if its first start fails.
            let clock = Arc::new(create_clock()?);
            self.lock().insert(
                display_id,
                DisplayEntry {
                    clock,
                    running: false,
                    subscribers: Vec::new(),
                },
            );
        }
        let clock_to_start = {
            let mut displays = self.lock();
            let entry = displays.get_mut(&display_id).unwrap();
            if entry
                .subscribers
                .iter()
                .any(|item| Arc::ptr_eq(item, source))
            {
                return Ok(());
            }
            entry.subscribers.push(source.clone());
            (!entry.running).then(|| entry.clock.clone())
        };
        if let Some(clock) = clock_to_start {
            let result = clock.start();
            let mut displays = self.lock();
            let entry = displays.get_mut(&display_id).unwrap();
            match result {
                Ok(()) => entry.running = true,
                Err(error) => {
                    entry.subscribers.retain(|item| !Arc::ptr_eq(item, source));
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn unsubscribe(
        &self,
        display_id: CGDirectDisplayID,
        source: &Arc<FrameRequests>,
    ) -> Result<()> {
        let clock_to_stop = {
            let mut displays = self.lock();
            let Some(entry) = displays.get_mut(&display_id) else {
                return Ok(());
            };
            entry.subscribers.retain(|item| !Arc::ptr_eq(item, source));
            (entry.subscribers.is_empty() && entry.running).then(|| entry.clock.clone())
        };
        if let Some(clock) = clock_to_stop {
            // On an unexpected stop error keep running=true, allowing the next
            // stop to retry. Subscribers are already gone, so no stale source
            // can be reached even when a final CV callback is still in flight.
            clock.stop()?;
            self.lock().get_mut(&display_id).unwrap().running = false;
        }
        Ok(())
    }

    fn request_frames(&self, display_id: CGDirectDisplayID) {
        if let Some(entry) = self.lock().get(&display_id) {
            for source in &entry.subscribers {
                unsafe { dispatch_source_merge_data(source.source, 1) };
            }
        }
    }
}

unsafe extern "C" fn display_link_callback(
    _display_link_out: *mut sys::CVDisplayLink,
    _current_time: *const sys::CVTimeStamp,
    _output_time: *const sys::CVTimeStamp,
    _flags_in: i64,
    _flags_out: *mut i64,
    display_id: *mut c_void,
) -> i32 {
    // The context is an integer, never a pointer to a window or dispatch source.
    REGISTRY.request_frames(display_id as usize as CGDirectDisplayID);
    0
}

struct FrameRequestContext {
    enabled: AtomicBool,
    data: *mut c_void,
    callback: unsafe extern "C" fn(*mut c_void),
}

// The immutable raw pointer is only interpreted by the target-queue handler;
// sharing the context itself only reads immutable fields or its atomic flag.
unsafe impl Send for FrameRequestContext {}
unsafe impl Sync for FrameRequestContext {}

struct FrameRequests {
    source: dispatch_source_t,
    context: Arc<FrameRequestContext>,
}

// The source is a thread-safe GCD object; enabled is atomic. The data pointer is
// dereferenced only by its handler on the target queue (the main queue in GPUI).
unsafe impl Send for FrameRequests {}
unsafe impl Sync for FrameRequests {}

impl FrameRequests {
    fn new(
        queue: dispatch_queue_t,
        data: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void),
    ) -> Result<Arc<Self>> {
        let source =
            unsafe { dispatch_source_create(&_dispatch_source_type_data_add, 0, 0, queue) };
        anyhow::ensure!(
            !source.is_null(),
            "could not create display link dispatch source"
        );
        let context = Arc::new(FrameRequestContext {
            enabled: AtomicBool::new(false),
            data,
            callback,
        });
        unsafe {
            let object = dispatch_object_t { _ds: source };
            dispatch_set_context(object, Arc::into_raw(context.clone()) as *mut c_void);
            dispatch_source_set_event_handler_f(source, Some(deliver_frame));
            dispatch_set_finalizer_f(object, Some(release_frame_context));
            // Activate exactly once, including sources whose clock setup fails.
            // Releasing an inactive/suspended dispatch source can crash libdispatch.
            dispatch_resume(object);
        }
        Ok(Arc::new(Self { source, context }))
    }

    fn set_enabled(&self, enabled: bool) {
        self.context.enabled.store(enabled, Ordering::Release);
    }
}

impl Drop for FrameRequests {
    fn drop(&mut self) {
        self.set_enabled(false);
        unsafe {
            dispatch_source_cancel(self.source);
            dispatch_release(dispatch_object_t { _ds: self.source });
        }
    }
}

unsafe extern "C" fn deliver_frame(context: *mut c_void) {
    let context = unsafe { &*context.cast::<FrameRequestContext>() };
    if context.enabled.load(Ordering::Acquire) {
        unsafe { (context.callback)(context.data) };
    }
}

unsafe extern "C" fn release_frame_context(context: *mut c_void) {
    // The dispatch finalizer runs after all handlers, including one which drops
    // its own DisplayLink, have finished. Keep their context valid until then.
    drop(unsafe { Arc::from_raw(context.cast::<FrameRequestContext>()) });
}

fn assert_main_thread() {
    use objc::{
        class, msg_send,
        runtime::{BOOL, YES},
        sel, sel_impl,
    };
    let is_main_thread: BOOL = unsafe { msg_send![class!(NSThread), isMainThread] };
    assert!(
        is_main_thread == YES,
        "display links must be managed on the main thread"
    );
}

pub struct DisplayLink {
    display_id: CGDirectDisplayID,
    frame_requests: Arc<FrameRequests>,
    started: bool,
    // Main-thread mutation is required both for registry transitions and for
    // cancellation to serialize with the handler using the native view pointer.
    _main_thread: PhantomData<Rc<()>>,
}

impl DisplayLink {
    pub fn new(
        display_id: CGDirectDisplayID,
        data: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void),
    ) -> Result<Self> {
        assert_main_thread();
        Ok(Self {
            display_id,
            frame_requests: FrameRequests::new(dispatch_get_main_queue(), data, callback)?,
            started: false,
            _main_thread: PhantomData,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        assert_main_thread();
        if self.started {
            return Ok(());
        }
        REGISTRY.subscribe(self.display_id, &self.frame_requests, || {
            Ok(CoreVideoClock(unsafe {
                sys::DisplayLink::new(
                    self.display_id,
                    display_link_callback,
                    self.display_id as usize as *mut c_void,
                )?
            }))
        })?;
        self.frame_requests.set_enabled(true);
        self.started = true;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        assert_main_thread();
        self.frame_requests.set_enabled(false);
        self.started = false;
        REGISTRY.unsubscribe(self.display_id, &self.frame_requests)
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.stop().log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Weak, atomic::AtomicUsize, mpsc},
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct ClockState {
        starts: AtomicUsize,
        stops: AtomicUsize,
        fail_start: AtomicBool,
        fail_stop: AtomicBool,
    }

    struct TestClock {
        state: Arc<ClockState>,
        registry: Weak<Registry<TestClock>>,
    }

    impl Clock for TestClock {
        fn start(&self) -> Result<()> {
            assert!(self.registry.upgrade().unwrap().displays.try_lock().is_ok());
            self.state.starts.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(
                !self.state.fail_start.swap(false, Ordering::SeqCst),
                "start failed"
            );
            Ok(())
        }

        fn stop(&self) -> Result<()> {
            assert!(self.registry.upgrade().unwrap().displays.try_lock().is_ok());
            self.state.stops.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(
                !self.state.fail_stop.swap(false, Ordering::SeqCst),
                "stop failed"
            );
            Ok(())
        }
    }

    fn clock(registry: &Arc<Registry<TestClock>>, state: &Arc<ClockState>) -> TestClock {
        assert!(registry.displays.try_lock().is_ok());
        TestClock {
            state: state.clone(),
            registry: Arc::downgrade(registry),
        }
    }

    unsafe extern "C" fn no_frame(_: *mut c_void) {}

    fn source() -> Arc<FrameRequests> {
        FrameRequests::new(
            unsafe { crate::dispatch_sys::dispatch_get_global_queue(0, 0) },
            std::ptr::null_mut(),
            no_frame,
        )
        .unwrap()
    }

    fn wait_for_context_release(context: Weak<FrameRequestContext>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while context.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "GCD source did not finalize its context"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn unstarted_source_releases_its_dispatch_context() {
        let source = source();
        let context = Arc::downgrade(&source.context);
        drop(source);
        wait_for_context_release(context);
    }

    #[test]
    fn clock_setup_and_start_failures_can_retry_without_retaining_subscribers() {
        let registry = Arc::new(Registry::new());
        let state = Arc::new(ClockState::default());
        let source = source();
        let context = Arc::downgrade(&source.context);

        assert!(
            registry
                .subscribe(1, &source, || anyhow::bail!("setup failed"))
                .is_err()
        );
        assert!(registry.lock().is_empty());
        assert_eq!(Arc::strong_count(&source), 1);

        state.fail_start.store(true, Ordering::SeqCst);
        assert!(
            registry
                .subscribe(1, &source, || Ok(clock(&registry, &state)))
                .is_err()
        );
        assert_eq!(registry.lock().len(), 1);
        assert!(!registry.lock()[&1].running);
        assert!(registry.lock()[&1].subscribers.is_empty());
        assert_eq!(Arc::strong_count(&source), 1);

        registry
            .subscribe(1, &source, || {
                panic!("must reuse the clock after a failed start")
            })
            .unwrap();
        assert_eq!(state.starts.load(Ordering::SeqCst), 2);
        registry.unsubscribe(1, &source).unwrap();
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
        drop(source);
        wait_for_context_release(context);
    }

    #[test]
    fn windows_share_one_clock_across_repeated_subscriptions() {
        let registry = Arc::new(Registry::new());
        let state = Arc::new(ClockState::default());
        let overlay = source();
        registry
            .subscribe(1, &overlay, || Ok(clock(&registry, &state)))
            .unwrap();
        // Repeated start is a no-op, rather than adding another subscriber.
        registry.subscribe(1, &overlay, || unreachable!()).unwrap();
        assert_eq!(registry.lock()[&1].subscribers.len(), 1);

        for _ in 0..10 {
            let settings = source();
            let context = Arc::downgrade(&settings.context);
            registry
                .subscribe(1, &settings, || panic!("same display must reuse its clock"))
                .unwrap();
            assert_eq!(registry.lock()[&1].subscribers.len(), 2);
            registry.unsubscribe(1, &settings).unwrap();
            registry.unsubscribe(1, &settings).unwrap();
            // A late tick can still reach the overlay, never the retired source.
            registry.request_frames(1);
            assert_eq!(Arc::strong_count(&settings), 1);
            drop(settings);
            wait_for_context_release(context);
            assert_eq!(state.starts.load(Ordering::SeqCst), 1);
            assert_eq!(state.stops.load(Ordering::SeqCst), 0);
        }

        registry.unsubscribe(1, &overlay).unwrap();
        registry.request_frames(1);
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
        registry.subscribe(1, &overlay, || unreachable!()).unwrap();
        assert_eq!(state.starts.load(Ordering::SeqCst), 2);
        registry.unsubscribe(1, &overlay).unwrap();
        assert_eq!(state.stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_stop_detaches_subscribers_and_retries_without_restarting_the_clock() {
        let registry = Arc::new(Registry::new());
        let state = Arc::new(ClockState::default());
        let source = source();
        registry
            .subscribe(1, &source, || Ok(clock(&registry, &state)))
            .unwrap();
        state.fail_stop.store(true, Ordering::SeqCst);
        assert!(registry.unsubscribe(1, &source).is_err());
        assert!(registry.lock()[&1].subscribers.is_empty());
        assert_eq!(Arc::strong_count(&source), 1);
        registry.request_frames(1);
        assert!(registry.lock()[&1].running);

        registry.unsubscribe(1, &source).unwrap();
        registry.unsubscribe(1, &source).unwrap();
        assert!(!registry.lock()[&1].running);
        assert_eq!(state.starts.load(Ordering::SeqCst), 1);
        assert_eq!(state.stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn frame_callback_can_drop_its_last_source_owner() {
        struct Owner {
            source: Mutex<Option<Arc<FrameRequests>>>,
            finished: mpsc::Sender<()>,
        }
        unsafe extern "C" fn close_from_frame(data: *mut c_void) {
            let owner = unsafe { &*data.cast::<Owner>() };
            drop(owner.source.lock().unwrap().take());
            let _ = owner.finished.send(());
        }
        let (finished, received) = mpsc::channel();
        // Keep callback data alive even if a timeout fails this test before GCD
        // has invoked the handler; reclaim it only after the source finalizes.
        let owner = Box::leak(Box::new(Owner {
            source: Mutex::new(None),
            finished,
        }));
        let source = FrameRequests::new(
            unsafe { crate::dispatch_sys::dispatch_get_global_queue(0, 0) },
            &*owner as *const Owner as *mut c_void,
            close_from_frame,
        )
        .unwrap();
        let context = Arc::downgrade(&source.context);
        source.set_enabled(true);
        let native_source = source.source;
        *owner.source.lock().unwrap() = Some(source);
        unsafe { dispatch_source_merge_data(native_source, 1) };
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        wait_for_context_release(context);
        assert!(owner.source.lock().unwrap().is_none());
        drop(unsafe { Box::from_raw(owner as *mut Owner) });
    }
}

mod sys {
    //! Derived from display-link crate under the following license:
    //! <https://github.com/BrainiumLLC/display-link/blob/master/LICENSE-MIT>
    //! Apple docs: [CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylinkoutputcallback?language=objc)
    #![allow(dead_code, non_upper_case_globals)]

    use anyhow::Result;
    use core_graphics::display::CGDirectDisplayID;
    use foreign_types::{ForeignType, foreign_type};
    use std::{
        ffi::c_void,
        fmt::{self, Debug, Formatter},
    };

    #[derive(Debug)]
    pub enum CVDisplayLink {}

    foreign_type! {
        pub unsafe type DisplayLink {
            type CType = CVDisplayLink;
            fn drop = CVDisplayLinkRelease;
            fn clone = CVDisplayLinkRetain;
        }
    }

    impl Debug for DisplayLink {
        fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
            formatter
                .debug_tuple("DisplayLink")
                .field(&self.as_ptr())
                .finish()
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    pub type CVTimeStampFlags = u64;

    pub const kCVTimeStampVideoTimeValid: CVTimeStampFlags = 1 << 0;
    pub const kCVTimeStampHostTimeValid: CVTimeStampFlags = 1 << 1;
    pub const kCVTimeStampSMPTETimeValid: CVTimeStampFlags = 1 << 2;
    pub const kCVTimeStampVideoRefreshPeriodValid: CVTimeStampFlags = 1 << 3;
    pub const kCVTimeStampRateScalarValid: CVTimeStampFlags = 1 << 4;
    pub const kCVTimeStampTopField: CVTimeStampFlags = 1 << 16;
    pub const kCVTimeStampBottomField: CVTimeStampFlags = 1 << 17;
    pub const kCVTimeStampVideoHostTimeValid: CVTimeStampFlags =
        kCVTimeStampVideoTimeValid | kCVTimeStampHostTimeValid;
    pub const kCVTimeStampIsInterlaced: CVTimeStampFlags =
        kCVTimeStampTopField | kCVTimeStampBottomField;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub time_type: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    pub type CVSMPTETimeType = u32;

    pub const kCVSMPTETimeType24: CVSMPTETimeType = 0;
    pub const kCVSMPTETimeType25: CVSMPTETimeType = 1;
    pub const kCVSMPTETimeType30Drop: CVSMPTETimeType = 2;
    pub const kCVSMPTETimeType30: CVSMPTETimeType = 3;
    pub const kCVSMPTETimeType2997: CVSMPTETimeType = 4;
    pub const kCVSMPTETimeType2997Drop: CVSMPTETimeType = 5;
    pub const kCVSMPTETimeType60: CVSMPTETimeType = 6;
    pub const kCVSMPTETimeType5994: CVSMPTETimeType = 7;

    pub type CVSMPTETimeFlags = u32;

    pub const kCVSMPTETimeValid: CVSMPTETimeFlags = 1 << 0;
    pub const kCVSMPTETimeRunning: CVSMPTETimeFlags = 1 << 1;

    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        display_link_out: *mut CVDisplayLink,
        // A pointer to the current timestamp. This represents the timestamp when the callback is called.
        current_time: *const CVTimeStamp,
        // A pointer to the output timestamp. This represents the timestamp for when the frame will be displayed.
        output_time: *const CVTimeStamp,
        // Unused
        flags_in: i64,
        // Unused
        flags_out: *mut i64,
        // A pointer to app-defined data.
        display_link_context: *mut c_void,
    ) -> i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CoreVideo", kind = "framework")]
    #[allow(improper_ctypes, unknown_lints, clippy::duplicated_attributes)]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(
            display_link_out: *mut *mut CVDisplayLink,
        ) -> i32;
        pub fn CVDisplayLinkSetCurrentCGDisplay(
            display_link: &mut DisplayLinkRef,
            display_id: u32,
        ) -> i32;
        pub fn CVDisplayLinkSetOutputCallback(
            display_link: &mut DisplayLinkRef,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> i32;
        pub fn CVDisplayLinkStart(display_link: &DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkStop(display_link: &DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkRelease(display_link: *mut CVDisplayLink);
        pub fn CVDisplayLinkRetain(display_link: *mut CVDisplayLink) -> *mut CVDisplayLink;
    }

    impl DisplayLink {
        /// Apple docs: [CVDisplayLinkCreateWithCGDisplay](https://developer.apple.com/documentation/corevideo/1456981-cvdisplaylinkcreatewithcgdisplay?language=objc)
        pub unsafe fn new(
            display_id: CGDirectDisplayID,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> Result<Self> {
            unsafe {
                let mut display_link: *mut CVDisplayLink = 0 as _;

                let code = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
                anyhow::ensure!(code == 0, "could not create display link, code: {}", code);

                let mut display_link = DisplayLink::from_ptr(display_link);

                let code = CVDisplayLinkSetOutputCallback(&mut display_link, callback, user_info);
                anyhow::ensure!(code == 0, "could not set output callback, code: {}", code);

                let code = CVDisplayLinkSetCurrentCGDisplay(&mut display_link, display_id);
                anyhow::ensure!(
                    code == 0,
                    "could not assign display to display link, code: {}",
                    code
                );

                Ok(display_link)
            }
        }
    }

    impl DisplayLinkRef {
        /// Apple docs: [CVDisplayLinkStart](https://developer.apple.com/documentation/corevideo/1457193-cvdisplaylinkstart?language=objc)
        pub unsafe fn start(&self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStart(self);
                // kCVReturnDisplayLinkAlreadyRunning: CoreVideo may have restarted
                // the link after a display/session change.
                anyhow::ensure!(
                    code == 0 || code == -6671,
                    "could not start display link, code: {}",
                    code
                );
                Ok(())
            }
        }

        /// Apple docs: [CVDisplayLinkStop](https://developer.apple.com/documentation/corevideo/1457281-cvdisplaylinkstop?language=objc)
        pub unsafe fn stop(&self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStop(self);
                // kCVReturnDisplayLinkNotRunning: the display/session may have gone away.
                anyhow::ensure!(
                    code == 0 || code == -6672,
                    "could not stop display link, code: {}",
                    code
                );
                Ok(())
            }
        }
    }
}
