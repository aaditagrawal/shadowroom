//! Read-only software/X11 feasibility prototype. This is not the GPUI desktop.
use anyhow::{Context, Result, bail, ensure};
use font8x8::UnicodeFonts;
use image::{Rgb, RgbImage};
use shadowroom::{
    catalog::Catalog,
    indexer::{IndexEvent, IndexJob, IndexOptions, Indexer},
    media::{MediaPipeline, Preview, PreviewKind},
    model::Photo,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use x11rb::{
    connection::{Connection, RequestConnection},
    protocol::{Event, xproto::*},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

const HEADER: u32 = 72;
const FOOTER: u32 = 44;
const BACKGROUND: [u8; 3] = [20, 22, 25];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Grid,
    Loupe,
}

struct Options {
    folder: PathBuf,
    state: Option<PathBuf>,
    offscreen: Option<PathBuf>,
    width: u32,
    height: u32,
    mode: Mode,
    actual: bool,
    selected: usize,
    pan: (i32, i32),
    timeout: Duration,
    exit_after: Option<Duration>,
}

fn help() {
    println!(
        "Shadowroom CPU/X11 compatibility PROTOTYPE (read-only)\n\n\
Usage: shadowroom-compat-prototype PHOTO_FOLDER [OPTIONS]\n\n\
  --offscreen FILE.png    Save the same CPU-rendered scene without an X server\n\
  --mode grid|loupe       Initial view (default: grid)\n\
  --zoom fit|100          Loupe scale (100 means one source pixel per pixel)\n\
  --select N             Initial photo, numbered from 1\n\
  --pan X,Y              Initial loupe offset in screen pixels\n\
  --width N --height N   Canvas size (default: 1200 x 800; max: 4096)\n\
  --state-dir PATH       Separate catalog directory, outside PHOTO_FOLDER\n\
                         Default: disposable temporary directory\n\
  --timeout SECONDS      Headless indexing/decode timeout (default: 60)\n\
  --exit-after SECONDS   Automatically close the X11 window (smoke tests)\n\
  --help                 Show this help\n\n\
Keys: Left/Right or J/K select; Enter/Space switches grid/loupe;\n\
G grid; L loupe; F fit; 1 actual pixels; H/A left, D right, W up, S down;\n\
PageUp/PageDown page through grid; Q/Escape quit. Click selects a tile.\n\n\
Originals are never edited. No delete, rating, export or ingest actions.\n\
X11 uses a Rust socket connection and CPU pixels; no GPU or libxcb backend.\n\
This prototype assumes an sRGB display and uses basic bitmap text."
    );
}

fn options() -> Result<Option<Options>> {
    let mut args = env::args_os().skip(1);
    let mut result = Options {
        folder: PathBuf::new(),
        state: None,
        offscreen: None,
        width: 1200,
        height: 800,
        mode: Mode::Grid,
        actual: false,
        selected: 0,
        pan: (0, 0),
        timeout: Duration::from_secs(60),
        exit_after: None,
    };
    while let Some(arg) = args.next() {
        let flag = arg.to_string_lossy();
        if matches!(flag.as_ref(), "--help" | "-h") {
            help();
            return Ok(None);
        }
        if !flag.starts_with('-') {
            ensure!(
                result.folder.as_os_str().is_empty(),
                "Only one photo folder is accepted"
            );
            result.folder = arg.into();
            continue;
        }
        let value = args
            .next()
            .with_context(|| format!("Missing value for {flag}"))?;
        let number = || {
            value
                .to_string_lossy()
                .parse::<u32>()
                .with_context(|| format!("Invalid number for {flag}"))
        };
        match flag.as_ref() {
            "--state-dir" => result.state = Some(value.into()),
            "--offscreen" => result.offscreen = Some(value.into()),
            "--width" => result.width = number()?,
            "--height" => result.height = number()?,
            "--select" => {
                result.selected =
                    number()?.checked_sub(1).context("Selection starts at 1")? as usize
            }
            "--timeout" => result.timeout = Duration::from_secs(number()?.into()),
            "--exit-after" => result.exit_after = Some(Duration::from_secs(number()?.into())),
            "--mode" => {
                result.mode = match value.to_str() {
                    Some("grid") => Mode::Grid,
                    Some("loupe") => Mode::Loupe,
                    _ => bail!("Mode must be grid or loupe"),
                }
            }
            "--zoom" => {
                result.actual = match value.to_str() {
                    Some("fit") => false,
                    Some("100") => true,
                    _ => bail!("Zoom must be fit or 100"),
                }
            }
            "--pan" => {
                let value = value.to_string_lossy();
                let (x, y) = value.split_once(',').context("Pan must be X,Y")?;
                result.pan = (x.parse()?, y.parse()?);
            }
            _ => bail!("Unknown option {flag}; use --help"),
        }
    }
    ensure!(
        !result.folder.as_os_str().is_empty(),
        "Supply a photo folder; use --help"
    );
    ensure!(
        (320..=4096).contains(&result.width) && (240..=4096).contains(&result.height),
        "Canvas dimensions must be between 320x240 and 4096x4096"
    );
    result.folder = result.folder.canonicalize().context("Open photo folder")?;
    ensure!(result.folder.is_dir(), "Photo folder must be a directory");
    Ok(Some(result))
}

struct App {
    root: PathBuf,
    catalog: Catalog,
    job: Option<IndexJob>,
    media: MediaPipeline,
    photos: Vec<Photo>,
    selected: usize,
    mode: Mode,
    actual: bool,
    pan: (i32, i32),
    width: u32,
    height: u32,
    status: String,
    indexed: bool,
}

impl App {
    fn new(options: &Options, state: &Path) -> Result<Self> {
        ensure!(
            !future_path(state)?.starts_with(&options.folder),
            "State directory must be outside the read-only photo folder"
        );
        fs::create_dir_all(state)?;
        ensure!(
            !state.canonicalize()?.starts_with(&options.folder),
            "State directory must be outside the read-only photo folder"
        );
        let catalog = Catalog::open(&state.join("catalog.sqlite3"))?;
        let job = Indexer::start(
            catalog.clone(),
            options.folder.clone(),
            IndexOptions::default(),
        );
        let media = MediaPipeline::new(state.join("cache"), 256 * 1024 * 1024);
        // Disposable runs should not leave persistence workers recreating
        // temporary paths while the process is shutting down.
        media.set_disk_budget(0);
        Ok(Self {
            root: options.folder.clone(),
            catalog,
            job: Some(job),
            media,
            photos: Vec::new(),
            selected: options.selected,
            mode: options.mode,
            actual: options.actual,
            pan: options.pan,
            width: options.width,
            height: options.height,
            status: "Indexing photo folder...".into(),
            indexed: false,
        })
    }

    fn poll_index(&mut self) -> Result<()> {
        let Some(job) = &self.job else {
            return Ok(());
        };
        let finished = job.is_finished();
        while let Ok(event) = job.try_recv() {
            match event {
                IndexEvent::Error { path, message } => eprintln!("{}: {message}", path.display()),
                IndexEvent::Progress(progress) => {
                    self.status = format!(
                        "Indexing: {} photos, {} errors",
                        progress.indexed, progress.errors
                    )
                }
                _ => {}
            }
        }
        if finished {
            self.job.take().unwrap().join()?;
            self.photos = self.catalog.photos()?;
            self.photos
                .retain(|photo| photo.path.starts_with(&self.root));
            self.photos.sort_by(|a, b| a.path.cmp(&b.path));
            self.selected = self.selected.min(self.photos.len().saturating_sub(1));
            self.indexed = true;
            self.status = format!(
                "{} photos | read-only originals | software rendering",
                self.photos.len()
            );
        }
        Ok(())
    }

    fn columns(&self) -> usize {
        ((self.width.saturating_sub(32)) / 208).max(1) as usize
    }
    fn rows(&self) -> usize {
        ((self.height - HEADER - FOOTER) / 190).max(1) as usize
    }
    fn page(&self) -> std::ops::Range<usize> {
        let size = self.columns() * self.rows();
        let start = self.selected / size * size;
        start..(start + size).min(self.photos.len())
    }
    fn required(&self) -> Vec<(usize, PreviewKind)> {
        match self.mode {
            Mode::Grid => self.page().map(|i| (i, PreviewKind::Thumbnail)).collect(),
            Mode::Loupe if !self.photos.is_empty() => vec![(
                self.selected,
                if self.actual {
                    PreviewKind::Full
                } else {
                    PreviewKind::Fit
                },
            )],
            _ => Vec::new(),
        }
    }
    fn select(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.photos.len().saturating_sub(1));
        self.pan = (0, 0);
    }
    fn key(&mut self, key: u32) -> bool {
        match key {
            0xff1b | 0x71 | 0x51 => return false,
            0xff51 | 0x6a | 0x4a => self.select(-1),
            0xff53 | 0x6b | 0x4b => self.select(1),
            0xff55 => self.select(-((self.columns() * self.rows()) as isize)),
            0xff56 => self.select((self.columns() * self.rows()) as isize),
            0xff0d | 0x20 => {
                self.mode = if self.mode == Mode::Grid {
                    Mode::Loupe
                } else {
                    Mode::Grid
                }
            }
            0x67 | 0x47 => self.mode = Mode::Grid,
            0x6c | 0x4c => self.mode = Mode::Loupe,
            0x66 | 0x46 => {
                self.actual = false;
                self.pan = (0, 0);
            }
            0x31 => {
                self.mode = Mode::Loupe;
                self.actual = true;
                self.pan = (0, 0);
            }
            0x68 | 0x48 | 0x61 | 0x41 => self.pan.0 = self.pan.0.saturating_add(80),
            0x64 | 0x44 => self.pan.0 = self.pan.0.saturating_sub(80),
            0x77 | 0x57 | 0xff52 => self.pan.1 = self.pan.1.saturating_add(80),
            0x73 | 0x53 | 0xff54 => self.pan.1 = self.pan.1.saturating_sub(80),
            _ => {}
        }
        true
    }

    fn render(&self) -> (RgbImage, bool) {
        let mut frame = RgbImage::from_pixel(self.width, self.height, Rgb(BACKGROUND));
        let mut ready = true;
        rectangle(&mut frame, (0, 0, self.width, HEADER), [30, 33, 37]);
        text(
            &mut frame,
            20,
            18,
            "SHADOWROOM / CPU + X11 PROTOTYPE",
            [231, 231, 224],
            2,
        );
        text(&mut frame, 20, 48, &self.status, [153, 166, 163], 1);
        if self.photos.is_empty() {
            text(
                &mut frame,
                24,
                HEADER + 36,
                if self.indexed {
                    "No supported photos found."
                } else {
                    "Scanning in background..."
                },
                [215, 215, 208],
                2,
            );
        }
        for (index, kind) in self.required() {
            let photo = &self.photos[index];
            self.media.request_photo(photo, kind);
            let preview = self.media.get_photo(photo, kind);
            ready &= preview.is_some() || self.media.photo_error(photo, kind).is_some();
            let preview = preview.or_else(|| self.media.best_photo(photo));
            match self.mode {
                Mode::Grid => {
                    let n = index - self.page().start;
                    let x = 16 + (n % self.columns()) as u32 * 208;
                    let y = HEADER + 12 + (n / self.columns()) as u32 * 190;
                    rectangle(
                        &mut frame,
                        (x, y, 196, 178),
                        if index == self.selected {
                            [126, 172, 145]
                        } else {
                            [48, 51, 56]
                        },
                    );
                    rectangle(&mut frame, (x + 3, y + 3, 190, 150), [12, 14, 16]);
                    if let Some(preview) = preview {
                        draw_preview(
                            &mut frame,
                            &preview,
                            (x + 3, y + 3, 190, 150),
                            false,
                            (0, 0),
                        );
                    } else {
                        text(
                            &mut frame,
                            x + 12,
                            y + 70,
                            if self.media.photo_error(photo, kind).is_some() {
                                "DECODE FAILED"
                            } else {
                                "LOADING"
                            },
                            [190, 190, 183],
                            1,
                        );
                    }
                    text(
                        &mut frame,
                        x + 8,
                        y + 160,
                        &filename(photo).chars().take(22).collect::<String>(),
                        [230, 230, 220],
                        1,
                    );
                }
                Mode::Loupe => {
                    if let Some(preview) = preview {
                        draw_preview(
                            &mut frame,
                            &preview,
                            (0, HEADER, self.width, self.height - HEADER - FOOTER),
                            self.actual,
                            self.pan,
                        );
                    } else {
                        text(
                            &mut frame,
                            24,
                            HEADER + 36,
                            "Loading selected image...",
                            [220, 220, 210],
                            2,
                        );
                    }
                    if let Some(error) = self.media.photo_error(photo, kind) {
                        text(
                            &mut frame,
                            20,
                            HEADER + 12,
                            &format!("DECODE ERROR: {}", error.message)
                                .chars()
                                .take(120)
                                .collect::<String>(),
                            [242, 157, 136],
                            1,
                        );
                    }
                }
            }
        }
        rectangle(
            &mut frame,
            (0, self.height - FOOTER, self.width, FOOTER),
            [30, 33, 37],
        );
        let name = self
            .photos
            .get(self.selected)
            .map(filename)
            .unwrap_or_default();
        let label = format!(
            "{} / {}  {}  | {} {}",
            if self.photos.is_empty() {
                0
            } else {
                self.selected + 1
            },
            self.photos.len(),
            name,
            if self.mode == Mode::Grid {
                "GRID"
            } else {
                "LOUPE"
            },
            if self.actual { "100%" } else { "FIT" }
        );
        text(
            &mut frame,
            16,
            self.height - FOOTER + 7,
            &label,
            [218, 222, 212],
            1,
        );
        text(
            &mut frame,
            16,
            self.height - FOOTER + 25,
            "ARROWS select | ENTER grid/loupe | F fit | 1 actual | WASD pan | Q quit",
            [153, 166, 163],
            1,
        );
        (frame, ready)
    }
}

fn filename(photo: &Photo) -> String {
    photo
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn rectangle(frame: &mut RgbImage, area: (u32, u32, u32, u32), color: [u8; 3]) {
    let (x, y, w, h) = area;
    for row in y..y.saturating_add(h).min(frame.height()) {
        for col in x..x.saturating_add(w).min(frame.width()) {
            frame.put_pixel(col, row, Rgb(color));
        }
    }
}

fn text(frame: &mut RgbImage, x: u32, y: u32, value: &str, color: [u8; 3], scale: u32) {
    for (index, character) in value.chars().enumerate() {
        let at = x + index as u32 * 8 * scale;
        if at + 8 * scale > frame.width() {
            break;
        }
        if let Some(bitmap) = font8x8::BASIC_FONTS
            .get(character)
            .or_else(|| font8x8::BASIC_FONTS.get('?'))
        {
            for (row, bits) in bitmap.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        rectangle(
                            frame,
                            (at + col * scale, y + row as u32 * scale, scale, scale),
                            color,
                        );
                    }
                }
            }
        }
    }
}

fn draw_preview(
    frame: &mut RgbImage,
    preview: &Preview,
    area: (u32, u32, u32, u32),
    actual: bool,
    pan: (i32, i32),
) {
    let (x, y, w, h) = area;
    if preview.width == 0 || preview.height == 0 {
        return;
    }
    let scale = if actual {
        1.0
    } else {
        (w as f64 / preview.width as f64).min(h as f64 / preview.height as f64)
    };
    let left = x as f64 + (w as f64 - preview.width as f64 * scale) / 2.0 + pan.0 as f64;
    let top = y as f64 + (h as f64 - preview.height as f64 * scale) / 2.0 + pan.1 as f64;
    for dy in y..(y + h).min(frame.height()) {
        let sy = ((dy as f64 - top) / scale).floor();
        if sy < 0.0 || sy >= preview.height as f64 {
            continue;
        }
        for dx in x..(x + w).min(frame.width()) {
            let sx = ((dx as f64 - left) / scale).floor();
            if sx < 0.0 || sx >= preview.width as f64 {
                continue;
            }
            let offset = (sy as usize * preview.width as usize + sx as usize) * 4;
            let rgba = &preview.rgba[offset..offset + 4];
            let alpha = rgba[3] as u32;
            let mut rgb = [0; 3];
            for c in 0..3 {
                rgb[c] =
                    ((rgba[c] as u32 * alpha + BACKGROUND[c] as u32 * (255 - alpha)) / 255) as u8;
            }
            frame.put_pixel(dx, dy, Rgb(rgb));
        }
    }
}

fn offscreen(app: &mut App, output: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        app.poll_index()?;
        let (frame, ready) = app.render();
        if app.indexed && ready {
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            image::DynamicImage::ImageRgb8(frame)
                .write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png)?;
            let failures = app
                .required()
                .iter()
                .filter(|&&(i, k)| app.media.photo_error(&app.photos[i], k).is_some())
                .count();
            println!(
                "Rendered {}x{} {:?}: {} photos, {failures} decode errors -> {}",
                app.width,
                app.height,
                app.mode,
                app.photos.len(),
                output.display()
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Timed out waiting for indexing/decoding"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn x11(app: &mut App, exit_after: Option<Duration>) -> Result<()> {
    let (connection, screen_number) = RustConnection::connect(None)
        .context("Connect to X11; use --offscreen for headless rendering")?;
    let screen = &connection.setup().roots[screen_number];
    let window = connection.generate_id()?;
    let gc = connection.generate_id()?;
    connection
        .create_window(
            screen.root_depth,
            window,
            screen.root,
            0,
            0,
            app.width as u16,
            app.height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .background_pixel(screen.black_pixel)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::KEY_PRESS
                        | EventMask::BUTTON_PRESS
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        )?
        .check()?;
    connection
        .create_gc(gc, window, &CreateGCAux::new())?
        .check()?;
    connection.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Shadowroom CPU/X11 PROTOTYPE - read-only",
    )?;
    let protocols = connection
        .intern_atom(false, b"WM_PROTOCOLS")?
        .reply()?
        .atom;
    let delete = connection
        .intern_atom(false, b"WM_DELETE_WINDOW")?
        .reply()?
        .atom;
    connection.change_property32(
        PropMode::REPLACE,
        window,
        protocols,
        AtomEnum::ATOM,
        &[delete],
    )?;
    connection.map_window(window)?.check()?;
    connection.flush()?;
    let format = connection
        .setup()
        .pixmap_formats
        .iter()
        .find(|f| f.depth == screen.root_depth)
        .context("Unsupported X11 image depth")?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == screen.root_visual)
        .context("Missing root visual")?;
    ensure!(
        visual.class == VisualClass::TRUE_COLOR && matches!(format.bits_per_pixel, 16 | 24 | 32),
        "Prototype requires a 16/24/32-bit TrueColor X11 display"
    );
    let keymap = connection
        .get_keyboard_mapping(
            connection.setup().min_keycode,
            connection.setup().max_keycode - connection.setup().min_keycode + 1,
        )?
        .reply()?;
    let start = Instant::now();
    let mut running = true;
    let mut redraw = true;
    let mut decoded = 0;
    while running {
        redraw |= !app.indexed;
        app.poll_index()?;
        while let Some(event) = connection.poll_for_event()? {
            match event {
                Event::Expose(_) => redraw = true,
                Event::ConfigureNotify(e) => {
                    app.width = u32::from(e.width).clamp(320, 4096);
                    app.height = u32::from(e.height).clamp(240, 4096);
                    redraw = true;
                }
                Event::KeyPress(e) => {
                    let i = (e.detail.saturating_sub(connection.setup().min_keycode)) as usize
                        * keymap.keysyms_per_keycode as usize;
                    if let Some(&symbol) = keymap.keysyms.get(i) {
                        running = app.key(symbol);
                        redraw = true;
                    }
                }
                Event::ButtonPress(e) if app.mode == Mode::Grid && e.detail == 1 => {
                    if e.event_x < 16
                        || e.event_y < (HEADER + 12) as i16
                        || e.event_y as u32 >= app.height - FOOTER
                    {
                        continue;
                    }
                    let x = (e.event_x as i32 - 16).max(0) as usize / 208;
                    let y = (e.event_y as i32 - HEADER as i32 - 12).max(0) as usize / 190;
                    let index = app.page().start + y * app.columns() + x;
                    if x < app.columns() && index < app.photos.len() {
                        app.selected = index;
                        app.pan = (0, 0);
                        redraw = true;
                    }
                }
                Event::ClientMessage(e)
                    if e.type_ == protocols && e.data.as_data32()[0] == delete =>
                {
                    running = false
                }
                Event::DestroyNotify(_) => running = false,
                _ => {}
            }
        }
        let current = app.media.stats().decoded + app.media.stats().errors;
        if current != decoded {
            redraw = true;
            decoded = current;
        }
        if !app.indexed {
            redraw = true;
        }
        if redraw && running {
            let (frame, _) = app.render();
            upload(
                &connection,
                window,
                gc,
                &frame,
                screen.root_depth,
                format,
                visual,
            )?;
            redraw = false;
        }
        if exit_after.is_some_and(|limit| start.elapsed() >= limit) {
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    connection.free_gc(gc)?;
    connection.destroy_window(window)?;
    connection.flush()?;
    Ok(())
}

fn upload(
    connection: &RustConnection,
    window: Window,
    gc: Gcontext,
    frame: &RgbImage,
    depth: u8,
    format: &Format,
    visual: &Visualtype,
) -> Result<()> {
    let stride = (frame.width() as usize * format.bits_per_pixel as usize)
        .div_ceil(format.scanline_pad as usize)
        * (format.scanline_pad as usize / 8);
    let bytes_per_pixel = (format.bits_per_pixel / 8) as usize;
    let rows = ((connection.maximum_request_bytes().saturating_sub(64)) / stride)
        .max(1)
        .min(frame.height() as usize);
    let lsb = connection.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
    for start in (0..frame.height() as usize).step_by(rows) {
        let count = rows.min(frame.height() as usize - start);
        let mut bytes = vec![0; stride * count];
        for row in 0..count {
            for x in 0..frame.width() as usize {
                let rgb = frame.get_pixel(x as u32, (start + row) as u32).0;
                let pixel = masks.iter().enumerate().fold(0u32, |out, (c, &mask)| {
                    out | (((rgb[c] as u32 * (mask >> mask.trailing_zeros()) + 127) / 255)
                        << mask.trailing_zeros())
                });
                let packed = if lsb {
                    pixel.to_le_bytes()
                } else {
                    pixel.to_be_bytes()
                };
                let data = if lsb {
                    &packed[..bytes_per_pixel]
                } else {
                    &packed[4 - bytes_per_pixel..]
                };
                bytes[row * stride + x * bytes_per_pixel..row * stride + (x + 1) * bytes_per_pixel]
                    .copy_from_slice(data);
            }
        }
        connection
            .put_image(
                ImageFormat::Z_PIXMAP,
                window,
                gc,
                frame.width() as u16,
                count as u16,
                0,
                start as i16,
                0,
                depth,
                &bytes,
            )?
            .check()?;
    }
    connection.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    let temporary = if options.state.is_none() {
        Some(
            tempfile::Builder::new()
                .prefix("shadowroom-compat-")
                .tempdir()?,
        )
    } else {
        None
    };
    let state = options
        .state
        .as_deref()
        .unwrap_or_else(|| temporary.as_ref().unwrap().path());
    if let Some(output) = &options.offscreen {
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        ensure!(
            !parent.canonicalize()?.starts_with(&options.folder),
            "Offscreen output must be outside the read-only photo folder"
        );
        ensure!(
            !output.exists(),
            "Refusing to overwrite an existing offscreen output"
        );
    }
    let mut app = App::new(&options, state)?;
    if let Some(output) = options.offscreen {
        offscreen(&mut app, &output, options.timeout)
    } else {
        x11(&mut app, options.exit_after)
    }
}

// Resolve existing ancestors before creating state, so a rejected state-dir
// inside the photo folder never creates anything there (including via links).
fn future_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    for ancestor in absolute.ancestors() {
        if ancestor.exists() {
            let mut resolved = ancestor.canonicalize()?;
            for component in absolute.strip_prefix(ancestor)?.components() {
                match component {
                    std::path::Component::Normal(name) => resolved.push(name),
                    std::path::Component::ParentDir => {
                        resolved.pop();
                    }
                    std::path::Component::CurDir => {}
                    _ => bail!("Invalid state directory"),
                }
            }
            return Ok(resolved);
        }
    }
    bail!("Cannot resolve state directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loupe_fit_and_actual_pixels_pan_and_clip_without_resizing_source() {
        let preview = Preview {
            width: 2,
            height: 1,
            source_width: 2,
            source_height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            kind: PreviewKind::Full,
            histogram: Default::default(),
            dhash: 0,
            color_status: shadowroom::media::ColorStatus::AssumedSrgb,
            embedded_raw: false,
            rendered_raw: false,
            rendering_note: None,
        };
        let mut frame = RgbImage::from_pixel(4, 4, Rgb(BACKGROUND));
        draw_preview(&mut frame, &preview, (0, 0, 4, 4), false, (0, 0));
        assert_eq!(frame.get_pixel(0, 1).0, [255, 0, 0]);
        assert_eq!(frame.get_pixel(3, 2).0, [0, 255, 0]);
        assert_eq!(frame.get_pixel(0, 0).0, BACKGROUND);
        let mut actual = RgbImage::from_pixel(4, 3, Rgb(BACKGROUND));
        draw_preview(&mut actual, &preview, (0, 0, 4, 3), true, (1, 0));
        assert_eq!(actual.get_pixel(2, 1).0, [255, 0, 0]);
        assert_eq!(actual.get_pixel(3, 1).0, [0, 255, 0]);
        assert_eq!(preview.rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
