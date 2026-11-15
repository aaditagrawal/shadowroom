//! Opt-in frame and input-latency trace.
//!
//! `SHADOWROOM_FRAME_TRACE=/path/trace.jsonl` appends one JSON object per line;
//! unset, [`Trace::from_env`] returns a disabled handle and every hook is one
//! `Option` check. `scripts/native-responsiveness.py` drives the app and reads
//! the file. Timestamps are `CLOCK_MONOTONIC` nanoseconds (the clock Python's
//! `time.monotonic_ns()` reads on Linux), so a driver can relate its own input
//! send times to these records. `cpu_*` fields are main-thread CPU time
//! (`CLOCK_THREAD_CPUTIME_ID`), which excludes time the thread was preempted.
//!
//! How the records line up with GPUI 0.2.2's frame loop: the platform calls
//! `request_frame` once per display interval (an X11 timer at the RandR refresh
//! rate, Wayland `wl_surface.frame` callbacks, a macOS `CVDisplayLink`). When the
//! window is dirty it runs `Window::draw` (which calls `Workspace::render`, then
//! lays out, prepaints and paints the whole tree) and `Window::present` (scene
//! submission to the GPU) inside one `App::update`. Effects deferred during
//! render flush at the end of that update, right after `present` returns, so
//! the `present` time here is "frame submitted", not photons on the screen.
//! GPUI also draws (without presenting) before dispatching a keystroke to a
//! dirty window, so its key bindings match the latest tree; such frames are
//! written with `"presented":false`. Keystrokes are stamped by a keystroke
//! interceptor, which runs after that draw and before any handler.

use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    rc::Rc,
};

use gpui::{
    AnyElement, App, Bounds, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Styled, Window,
};

fn clock(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime only writes the provided timespec.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// `CLOCK_MONOTONIC` in nanoseconds.
fn now_ns() -> u64 {
    clock(libc::CLOCK_MONOTONIC)
}

/// This thread's CPU time in nanoseconds.
fn cpu_ns() -> u64 {
    clock(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// Phase costs of one region: request_layout, prepaint, paint (CPU ns).
type RegionCost = [u64; 3];

#[derive(Default)]
struct Pending {
    /// A frame was rendered and its deferred present stamp has not run yet.
    open: bool,
    /// A keystroke was dispatched in between: GPUI drew to dispatch, not to present.
    dispatched: bool,
    cpu_start: u64,
    /// Stamped by [`Trace::probe`] during prepaint and paint.
    prepainted: u64,
    painted: u64,
    cpu_painted: u64,
    regions: Vec<(&'static str, RegionCost)>,
}

struct Recorder {
    out: BufWriter<File>,
    frames: u64,
    pending: Pending,
    last_flush: u64,
}

impl Recorder {
    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        let _ = self.out.write_fmt(args);
        let _ = self.out.write_all(b"\n");
    }
    fn region(&mut self, name: &'static str, phase: usize, cost: u64) {
        let regions = &mut self.pending.regions;
        match regions.iter_mut().find(|(n, _)| *n == name) {
            Some((_, costs)) => costs[phase] += cost,
            None => {
                let mut costs = [0; 3];
                costs[phase] = cost;
                regions.push((name, costs));
            }
        }
    }
}

/// What a frame drew, recorded beside its timings.
pub(super) struct FrameInfo {
    pub view: &'static str,
    pub overlay: &'static str,
    /// Photo textures the frame placed (grid cells, filmstrip, loupe panes).
    pub textures: usize,
    /// The repaint was requested by the background poll rather than input.
    pub poll: bool,
}

/// A cheap handle; clones share one output file.
#[derive(Clone, Default)]
pub(super) struct Trace(Option<Rc<RefCell<Recorder>>>);

impl Trace {
    pub fn from_env() -> Self {
        let Some(path) = std::env::var_os("SHADOWROOM_FRAME_TRACE") else {
            return Self(None);
        };
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => {
                let mut recorder = Recorder {
                    out: BufWriter::with_capacity(64 * 1024, file),
                    frames: 0,
                    pending: Pending::default(),
                    last_flush: now_ns(),
                };
                recorder.line(format_args!(
                    r#"{{"k":"start","t":{},"pid":{}}}"#,
                    now_ns(),
                    std::process::id()
                ));
                Self(Some(Rc::new(RefCell::new(recorder))))
            }
            Err(error) => {
                eprintln!("Frame trace unavailable: {error}");
                Self(None)
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// An input event, stamped before the workspace handles it.
    pub fn input(&self, kind: &str, detail: &str) {
        if let Some(recorder) = &self.0 {
            let detail = serde_json::to_string(detail).unwrap_or_default();
            let mut recorder = recorder.borrow_mut();
            if recorder.pending.open {
                recorder.pending.dispatched = true;
            }
            recorder.line(format_args!(
                r#"{{"k":"input","t":{},"kind":"{kind}","detail":{detail}}}"#,
                now_ns()
            ));
        }
    }

    /// Input dispatch finished (every handler for the event has returned).
    pub fn handled(&self) {
        if let Some(recorder) = &self.0 {
            recorder
                .borrow_mut()
                .line(format_args!(r#"{{"k":"handled","t":{}}}"#, now_ns()));
        }
    }

    /// Start of `Workspace::render`; `None` when tracing is off.
    pub fn begin(&self) -> Option<u64> {
        let recorder = self.0.as_ref()?;
        let mut recorder = recorder.borrow_mut();
        recorder.pending = Pending {
            open: true,
            cpu_start: cpu_ns(),
            ..Pending::default()
        };
        Some(now_ns())
    }

    /// End of `Workspace::render`. The `present` time is stamped by an effect
    /// that GPUI flushes after this frame's draw and present return.
    pub fn end(&self, started: Option<u64>, info: FrameInfo, cx: &mut App) {
        let (Some(recorder), Some(started)) = (&self.0, started) else {
            return;
        };
        let rendered = now_ns();
        let frame = {
            let mut recorder = recorder.borrow_mut();
            recorder.frames += 1;
            recorder.frames
        };
        let recorder = recorder.clone();
        cx.defer(move |_| {
            let presented = now_ns();
            let mut recorder = recorder.borrow_mut();
            let pending = std::mem::take(&mut recorder.pending);
            let mut regions = String::new();
            for (name, [layout, prepaint, paint]) in &pending.regions {
                if !regions.is_empty() {
                    regions.push(',');
                }
                regions.push_str(&format!(r#""{name}":[{layout},{prepaint},{paint}]"#));
            }
            recorder.line(format_args!(
                r#"{{"k":"frame","n":{frame},"render_start":{started},"render_end":{rendered},"prepainted":{},"painted":{},"present":{presented},"cpu_ns":{},"presented":{},"view":"{}","overlay":"{}","textures":{},"poll":{},"regions":{{{regions}}}}}"#,
                pending.prepainted,
                pending.painted,
                pending.cpu_painted.saturating_sub(pending.cpu_start),
                !pending.dispatched,
                info.view,
                info.overlay,
                info.textures,
                info.poll
            ));
            // Flush at most four times a second so tracing barely perturbs frames.
            if presented.saturating_sub(recorder.last_flush) > 250_000_000 {
                recorder.last_flush = presented;
                let _ = recorder.out.flush();
            }
        });
    }

    /// An empty element for the end of the root: its prepaint and paint run
    /// after layout and after every earlier sibling has painted, splitting a
    /// frame into render, layout + prepaint, paint and present.
    pub fn probe(&self) -> Option<AnyElement> {
        let recorder = self.0.clone()?;
        let painted = recorder.clone();
        Some(
            gpui::canvas(
                move |_, _, _| recorder.borrow_mut().pending.prepainted = now_ns(),
                move |_, _, _, _| {
                    let mut recorder = painted.borrow_mut();
                    recorder.pending.painted = now_ns();
                    recorder.pending.cpu_painted = cpu_ns();
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    /// Wraps a region of the tree so each frame records the main-thread CPU
    /// time its request_layout, prepaint and paint took. Taffy's layout pass
    /// over the whole tree is shared and not attributed. Untraced, the element
    /// passes through untouched.
    pub fn timed(&self, name: &'static str, element: impl IntoElement) -> AnyElement {
        match &self.0 {
            Some(recorder) => Timed {
                name,
                child: element.into_any_element(),
                recorder: recorder.clone(),
            }
            .into_any_element(),
            None => element.into_any_element(),
        }
    }

    pub fn flush(&self) {
        if let Some(recorder) = &self.0 {
            let _ = recorder.borrow_mut().out.flush();
        }
    }
}

struct Timed {
    name: &'static str,
    child: AnyElement,
    recorder: Rc<RefCell<Recorder>>,
}

impl Timed {
    fn measure<R>(&mut self, phase: usize, f: impl FnOnce(&mut AnyElement) -> R) -> R {
        let started = cpu_ns();
        let result = f(&mut self.child);
        let cost = cpu_ns().saturating_sub(started);
        self.recorder.borrow_mut().region(self.name, phase, cost);
        result
    }
}

impl IntoElement for Timed {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for Timed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (
            self.measure(0, |child| child.request_layout(window, cx)),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.measure(1, |child| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.measure(2, |child| child.paint(window, cx));
    }
}
