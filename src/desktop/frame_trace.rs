//! Opt-in frame and input-latency trace.
//!
//! `SHADOWROOM_FRAME_TRACE=/path/trace.jsonl` appends one JSON object per line;
//! unset, [`Trace::from_env`] returns a disabled handle and every hook is one
//! `Option` check. `scripts/native-responsiveness.py` drives the app and reads
//! the file. Timestamps are `CLOCK_MONOTONIC` nanoseconds (the clock Python's
//! `time.monotonic_ns()` reads on Linux), so a driver can relate its own input
//! send times to these records.
//!
//! How the records line up with GPUI 0.2.2's frame loop: the platform calls
//! `request_frame` once per display interval (an X11 timer at the RandR refresh
//! rate, Wayland `wl_surface.frame` callbacks, a macOS `CVDisplayLink`). When the
//! window is dirty it runs `Window::draw` (which calls `Workspace::render`, then
//! lays out, prepaints and paints the whole tree) and `Window::present` (scene
//! submission to the GPU) inside one `App::update`. Effects deferred during
//! render flush at the end of that update, right after `present` returns, so
//! the `present` time here is "frame submitted", not photons on the screen.
//! Keystrokes are stamped by a keystroke interceptor before any dispatch.

use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    rc::Rc,
};

/// `CLOCK_MONOTONIC` in nanoseconds.
pub(super) fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime only writes the provided timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

struct Recorder {
    out: BufWriter<File>,
    frames: u64,
    last_flush: u64,
}

impl Recorder {
    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        let _ = self.out.write_fmt(args);
        let _ = self.out.write_all(b"\n");
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
    pub fn input(&self, kind: &str, detail: &str, repeat: bool) {
        if let Some(recorder) = &self.0 {
            let detail = serde_json::to_string(detail).unwrap_or_default();
            recorder.borrow_mut().line(format_args!(
                r#"{{"k":"input","t":{},"kind":"{kind}","detail":{detail},"repeat":{repeat}}}"#,
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
        self.0.as_ref().map(|_| now_ns())
    }

    /// End of `Workspace::render`. The `present` time is stamped by an effect
    /// that GPUI flushes after this frame's draw and present return.
    pub fn end(&self, started: Option<u64>, info: FrameInfo, cx: &mut gpui::App) {
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
            recorder.line(format_args!(
                r#"{{"k":"frame","n":{frame},"render_start":{started},"render_end":{rendered},"present":{presented},"view":"{}","overlay":"{}","textures":{},"poll":{}}}"#,
                info.view, info.overlay, info.textures, info.poll
            ));
            // Flush at most four times a second so tracing barely perturbs frames.
            if presented.saturating_sub(recorder.last_flush) > 250_000_000 {
                recorder.last_flush = presented;
                let _ = recorder.out.flush();
            }
        });
    }

    pub fn flush(&self) {
        if let Some(recorder) = &self.0 {
            let _ = recorder.borrow_mut().out.flush();
        }
    }
}
