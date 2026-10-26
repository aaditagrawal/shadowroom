//! Full sensor RAW rendering through the public LibRaw C API.
//!
//! `libraw_data_t` stays opaque. Inspection identifies headers but never unpacks
//! pixels; callers must reserve `RawInfo::working_bytes` before decoding. This
//! is a conservative working-set estimate, not a native allocator sandbox.
//! LibRaw's memory writer applies orientation: NEVER apply EXIF orientation
//! again to the returned image. Thumbnail/fit tiers should use camera previews.
use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, GrayImage, ImageBuffer, Luma, Rgb, RgbImage};
use libloading::Library;
use std::{
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    marker::PhantomData,
    path::PathBuf,
    ptr::NonNull,
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_SOURCE: usize = 512 * 1024 * 1024;
const MAX_PIXELS: u64 = 160_000_000;
const FIXED_WORKING: u64 = 16 * 1024 * 1024;
const LIBRAW_IMAGE_BITMAP: c_int = 2;

#[derive(Clone, Debug)]
pub struct RawInfo {
    /// Expected output dimensions, including rotation and non-square pixels.
    pub width: u32,
    pub height: u32,
    /// Sensor allocation dimensions, including masked margins.
    pub raw_width: u32,
    pub raw_height: u32,
    /// Additional working set; excludes the caller-owned compressed source.
    /// Includes native buffers, output copies and downstream RGBA conversion.
    pub working_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawWhiteBalance {
    Camera,
    /// Missing/invalid as-shot multipliers: LibRaw's camera daylight defaults.
    DaylightFallback,
}

#[derive(Clone, Debug)]
pub struct RawProvenance {
    pub libraw_version: String,
    pub decoder: String,
    pub white_balance: RawWhiteBalance,
    pub camera_multipliers: Option<[f32; 4]>,
    pub converted_to_srgb: bool,
    pub orientation_applied: bool,
}

pub struct DecodedRaw {
    /// Eight-bit sRGB in display orientation; this is rendered sensor data.
    pub image: DynamicImage,
    pub source_width: u32,
    pub source_height: u32,
    pub provenance: RawProvenance,
}

type Raw = *mut c_void;
type Step = unsafe extern "C" fn(Raw) -> c_int;
type SetInt = unsafe extern "C" fn(Raw, c_int);
type SetFloat = unsafe extern "C" fn(Raw, f32);
type GetInt = unsafe extern "C" fn(Raw) -> c_int;
type Progress = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> c_int;

// This is the public, independently allocated libraw_processed_image_t prefix,
// NOT any part of libraw_data_t. The flexible data payload begins at offset 16
// (sizeof the C struct includes padding after its historical data[1] member).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MemHeader {
    kind: c_int,
    height: u16,
    width: u16,
    colors: u16,
    bits: u16,
    data_size: c_uint,
}

struct Api {
    _library: Library,
    // Unix's non-_r library can share native scratch state. Keep its fallback
    // useful without racing the application's two media workers.
    serial: Option<Mutex<()>>,
    init: unsafe extern "C" fn(c_uint) -> Raw,
    close: unsafe extern "C" fn(Raw),
    open: unsafe extern "C" fn(Raw, *const c_void, usize) -> c_int,
    adjust: Step,
    unpack: Step,
    process: Step,
    make_image: unsafe extern "C" fn(Raw, *mut c_int) -> *mut MemHeader,
    clear_image: unsafe extern "C" fn(*mut MemHeader),
    raw_width: GetInt,
    raw_height: GetInt,
    width: GetInt,
    height: GetInt,
    cam_mul: unsafe extern "C" fn(Raw, c_int) -> f32,
    user_mul: unsafe extern "C" fn(Raw, c_int, f32),
    gamma: unsafe extern "C" fn(Raw, c_int, f32),
    demosaic: SetInt,
    color: SetInt,
    bps: SetInt,
    auto_bright: SetInt,
    adjust_maximum: SetFloat,
    bright: SetFloat,
    highlight: SetInt,
    progress: unsafe extern "C" fn(Raw, Option<Progress>, *mut c_void),
    version: unsafe extern "C" fn() -> *const c_char,
    decoder: unsafe extern "C" fn(Raw) -> *const c_char,
    error: unsafe extern "C" fn(c_int) -> *const c_char,
    omp_get_threads: Option<unsafe extern "C" fn() -> c_int>,
    omp_set_threads: Option<unsafe extern "C" fn(c_int)>,
}

impl Api {
    unsafe fn load(path: &PathBuf) -> Result<Self> {
        // Keep the library alive as long as any copied symbol can be used.
        let lib = unsafe { Library::new(path) }?;
        let reentrant_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("libraw_r") || s.starts_with("raw_r"));
        macro_rules! symbol {
            ($name:literal) => {
                unsafe { *lib.get(concat!($name, "\0").as_bytes())? }
            };
        }
        Ok(Self {
            serial: (!reentrant_name).then(|| Mutex::new(())),
            init: symbol!("libraw_init"),
            close: symbol!("libraw_close"),
            open: symbol!("libraw_open_buffer"),
            adjust: symbol!("libraw_adjust_sizes_info_only"),
            unpack: symbol!("libraw_unpack"),
            process: symbol!("libraw_dcraw_process"),
            make_image: symbol!("libraw_dcraw_make_mem_image"),
            clear_image: symbol!("libraw_dcraw_clear_mem"),
            raw_width: symbol!("libraw_get_raw_width"),
            raw_height: symbol!("libraw_get_raw_height"),
            width: symbol!("libraw_get_iwidth"),
            height: symbol!("libraw_get_iheight"),
            cam_mul: symbol!("libraw_get_cam_mul"),
            user_mul: symbol!("libraw_set_user_mul"),
            gamma: symbol!("libraw_set_gamma"),
            demosaic: symbol!("libraw_set_demosaic"),
            color: symbol!("libraw_set_output_color"),
            bps: symbol!("libraw_set_output_bps"),
            auto_bright: symbol!("libraw_set_no_auto_bright"),
            // Released 0.20/0.21 symbol spelling differs from the master header.
            adjust_maximum: symbol!("libraw_set_adjust_maximum_thr"),
            bright: symbol!("libraw_set_bright"),
            highlight: symbol!("libraw_set_highlight"),
            progress: symbol!("libraw_set_progress_handler"),
            version: symbol!("libraw_version"),
            decoder: symbol!("libraw_unpack_function_name"),
            error: symbol!("libraw_strerror"),
            omp_get_threads: unsafe { lib.get(b"omp_get_max_threads\0").ok().map(|s| *s) },
            omp_set_threads: unsafe { lib.get(b"omp_set_num_threads\0").ok().map(|s| *s) },
            _library: lib,
        })
    }

    fn check(&self, code: c_int, operation: &str) -> Result<()> {
        if code != 0 {
            let message = unsafe { native_string((self.error)(code)) };
            bail!("LibRaw {operation}: {message} ({code})");
        }
        Ok(())
    }
}

fn candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    let names = [
        "libraw_r.so.25",
        "libraw_r.so.23",
        "libraw_r.so.20",
        "libraw_r.so",
        "libraw.so.25",
        "libraw.so.23",
        "libraw.so.20",
        "libraw.so",
    ];
    #[cfg(target_os = "macos")]
    let names = [
        "libraw_r.25.dylib",
        "libraw_r.23.dylib",
        "libraw_r.20.dylib",
        "libraw_r.dylib",
        "libraw.25.dylib",
        "libraw.23.dylib",
        "libraw.20.dylib",
        "libraw.dylib",
    ];
    #[cfg(target_os = "windows")]
    let names = [
        "libraw_r.dll",
        "libraw.dll",
        "raw_r.dll",
        "raw.dll",
        "libraw-25.dll",
        "libraw-23.dll",
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let names = ["libraw_r.so", "libraw.so"];
    let mut paths = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_owned()))
    {
        for base in [
            dir.clone(),
            dir.join("lib"),
            dir.join("../lib"),
            dir.join("../Frameworks"),
        ] {
            paths.extend(names.iter().map(|name| base.join(name)));
        }
    }
    #[cfg(target_os = "macos")]
    for base in [
        "/opt/homebrew/lib",
        "/usr/local/lib",
        "/opt/homebrew/opt/libraw/lib",
        "/usr/local/opt/libraw/lib",
    ] {
        paths.extend(names.iter().map(|name| PathBuf::from(base).join(name)));
    }
    paths.extend(names.iter().map(PathBuf::from));
    paths
}

fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        let mut errors = Vec::new();
        for path in candidates() {
            match unsafe { Api::load(&path) } {
                Ok(api) => return Ok(api),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        Err(format!(
            "Sensor RAW rendering needs a compatible LibRaw shared library: {}",
            errors.join("; ")
        ))
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!("{e}"))
}

// OpenMP controls are task/thread-local. No process-wide environment mutation.
struct NativeThreads<'a> {
    api: &'a Api,
    previous: Option<c_int>,
}
impl<'a> NativeThreads<'a> {
    fn single(api: &'a Api) -> Self {
        let previous = match (api.omp_get_threads, api.omp_set_threads) {
            (Some(get), Some(set)) => unsafe {
                let n = get();
                set(1);
                Some(n)
            },
            _ => None, // Serial LibRaw builds need no OpenMP control.
        };
        Self { api, previous }
    }
}
impl Drop for NativeThreads<'_> {
    fn drop(&mut self) {
        if let (Some(previous), Some(set)) = (self.previous, self.api.omp_set_threads) {
            unsafe { set(previous) };
        }
    }
}

struct Context<'a> {
    api: &'static Api,
    raw: NonNull<c_void>,
    // LibRaw's input stream borrows bytes and its callback borrows cancellation.
    _input: PhantomData<&'a [u8]>,
    _cancel: Option<&'a AtomicBool>,
    _serial: Option<MutexGuard<'static, ()>>,
}
impl<'a> Context<'a> {
    fn open(bytes: &'a [u8], cancel: Option<&'a AtomicBool>) -> Result<Self> {
        validate_source_len(bytes.len())?;
        check_cancel(cancel)?;
        let api = api()?;
        let serial = api
            .serial
            .as_ref()
            .map(|lock| lock.lock())
            .transpose()
            .map_err(|_| anyhow::anyhow!("LibRaw fallback lock poisoned"))?;
        let raw =
            NonNull::new(unsafe { (api.init)(0) }).context("LibRaw context allocation failed")?;
        let context = Self {
            api,
            raw,
            _input: PhantomData,
            _cancel: cancel,
            _serial: serial,
        };
        unsafe {
            (api.progress)(
                context.ptr(),
                Some(progress),
                cancel.map_or(std::ptr::null_mut(), |c| {
                    std::ptr::from_ref(c).cast_mut().cast()
                }),
            );
        }
        api.check(
            unsafe { (api.open)(context.ptr(), bytes.as_ptr().cast(), bytes.len()) },
            "identify",
        )?;
        check_cancel(cancel)?;
        Ok(context)
    }
    fn ptr(&self) -> Raw {
        self.raw.as_ptr()
    }
    fn info(&self) -> Result<RawInfo> {
        let raw_width = unsafe { (self.api.raw_width)(self.ptr()) };
        let raw_height = unsafe { (self.api.raw_height)(self.ptr()) };
        // Validate sensor dimensions even when a crop would make output small.
        validate_dimensions(raw_width, raw_height)?;
        self.api.check(
            unsafe { (self.api.adjust)(self.ptr()) },
            "inspect dimensions",
        )?;
        let width = unsafe { (self.api.width)(self.ptr()) };
        let height = unsafe { (self.api.height)(self.ptr()) };
        make_info(raw_width, raw_height, width, height)
    }
}
impl Drop for Context<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.ptr()) };
    }
}

unsafe extern "C" fn progress(data: *mut c_void, _: c_int, _: c_int, _: c_int) -> c_int {
    // No allocation, locking, panic or Rust unwinding through native frames.
    if data.is_null() {
        0
    } else {
        i32::from(unsafe { &*data.cast::<AtomicBool>() }.load(Ordering::Relaxed))
    }
}
fn check_cancel(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        bail!("Sensor RAW rendering cancelled");
    }
    Ok(())
}
fn validate_source_len(len: usize) -> Result<()> {
    if len < 16 {
        bail!("Truncated RAW source");
    }
    if len > MAX_SOURCE {
        bail!("RAW source exceeds the 512 MiB safety limit");
    }
    Ok(())
}
fn validate_dimensions(width: c_int, height: c_int) -> Result<u64> {
    if width <= 0 || height <= 0 || width > 65_535 || height > 65_535 {
        bail!("Invalid RAW dimensions");
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_PIXELS {
        bail!("RAW dimensions exceed the 160 megapixel safety limit");
    }
    Ok(pixels)
}
fn make_info(raw_width: c_int, raw_height: c_int, width: c_int, height: c_int) -> Result<RawInfo> {
    let sensor = validate_dimensions(raw_width, raw_height)?;
    let display = validate_dimensions(width, height)?;
    // Single-threaded decoder: raw planes/codec scratch + 4-channel image and
    // interpolation/rotation; separately account output/Rust/RGBA/derived copies.
    // inspect_inner separately adds file-sized metadata copies.
    let working = sensor
        .checked_mul(32)
        .and_then(|n| n.checked_add(display * 16))
        .and_then(|n| n.checked_add(FIXED_WORKING))
        .context("RAW allocation estimate overflow")?;
    Ok(RawInfo {
        width: width as u32,
        height: height as u32,
        raw_width: raw_width as u32,
        raw_height: raw_height as u32,
        working_bytes: usize::try_from(working)?,
    })
}

/// Reserve this additional metadata allowance before calling `inspect`.
/// Identification may copy profiles/maker notes, although it unpacks no pixels.
/// The compressed source itself must already be accounted for by the caller.
pub fn inspection_working_bytes(source_len: usize) -> Result<usize> {
    validate_source_len(source_len)?;
    source_len
        .checked_mul(2)
        .and_then(|n| n.checked_add(FIXED_WORKING as usize))
        .context("RAW metadata estimate overflow")
}

/// Identify the full-resolution sensor/output geometry without pixel unpacking.
/// Inspection uses its own context because size adjustment mutates Fuji state.
pub fn inspect(bytes: &[u8]) -> Result<RawInfo> {
    inspect_inner(bytes, None)
}
fn inspect_inner(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<RawInfo> {
    let context = Context::open(bytes, cancel)?;
    let mut info = context.info()?;
    info.working_bytes = info
        .working_bytes
        .checked_add(
            bytes
                .len()
                .checked_mul(2)
                .context("RAW metadata estimate overflow")?,
        )
        .context("RAW allocation estimate overflow")?;
    Ok(info)
}

pub fn decode(bytes: &[u8], admitted_working_bytes: usize) -> Result<DecodedRaw> {
    decode_inner(bytes, admitted_working_bytes, None)
}

/// Cancellation is cooperative at LibRaw progress callbacks and stage borders.
/// A codec without intermediate callbacks can finish its current stage first.
pub fn decode_with_cancel(
    bytes: &[u8],
    admitted_working_bytes: usize,
    cancel: &AtomicBool,
) -> Result<DecodedRaw> {
    decode_inner(bytes, admitted_working_bytes, Some(cancel))
}

fn decode_inner(
    bytes: &[u8],
    admitted_working_bytes: usize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedRaw> {
    let info = inspect_inner(bytes, cancel)?;
    if admitted_working_bytes < info.working_bytes {
        bail!(
            "Sensor RAW rendering needs {} working bytes; only {admitted_working_bytes} admitted",
            info.working_bytes
        );
    }
    // The inspection context is already closed. NEVER adjust sizes on this
    // context: unpack/process must start from the original sensor geometry.
    let context = Context::open(bytes, cancel)?;
    let api = context.api;
    let _threads = NativeThreads::single(api);
    let raw = context.ptr();
    let mut multipliers = std::array::from_fn(|i| unsafe { (api.cam_mul)(raw, i as c_int) });
    let camera_wb = sanitize_camera_multipliers(&mut multipliers);
    unsafe {
        (api.color)(raw, 1); // sRGB primaries, with camera matrix conversion.
        (api.bps)(raw, 8);
        (api.demosaic)(raw, 3); // AHD for Bayer; three passes for X-Trans.
        (api.auto_bright)(raw, 1);
        (api.adjust_maximum)(raw, 0.0); // Avoid scene-dependent white-level lift.
        (api.bright)(raw, 1.0);
        (api.highlight)(raw, 0); // Clip; no detail-inventing recovery.
        (api.gamma)(raw, 0, 1.0 / 2.4);
        (api.gamma)(raw, 1, 12.92); // sRGB transfer, not LibRaw's default BT.709.
        if camera_wb {
            for (i, multiplier) in multipliers.iter().enumerate() {
                (api.user_mul)(raw, i as c_int, *multiplier);
            }
        }
    }
    let decoder = unsafe { native_string((api.decoder)(raw)) };
    check_cancel(cancel)?;
    api.check(unsafe { (api.unpack)(raw) }, "unpack sensor")?;
    check_cancel(cancel)?;
    api.check(unsafe { (api.process)(raw) }, "demosaic sensor")?;
    check_cancel(cancel)?;
    let mut error = 0;
    let ptr = unsafe { (api.make_image)(raw, &mut error) };
    // Own a nonnull allocation even if the native function also reports error.
    let memory = NonNull::new(ptr).map(|ptr| MemoryImage { ptr, api });
    api.check(error, "make RGB image")?;
    let memory = memory.context("LibRaw produced no sensor image")?;
    let header = unsafe { *memory.ptr.as_ptr() };
    let count = validate_header(&header, &info)?;
    check_cancel(cancel)?;
    // Native result payload is contiguous and owned by `memory` until copied.
    let data = unsafe {
        std::slice::from_raw_parts(
            memory
                .ptr
                .as_ptr()
                .cast::<u8>()
                .add(std::mem::size_of::<MemHeader>()),
            count,
        )
    };
    let image = copy_pixels(&header, data)?;
    check_cancel(cancel)?;
    Ok(DecodedRaw {
        source_width: u32::from(header.width),
        source_height: u32::from(header.height),
        image,
        provenance: RawProvenance {
            libraw_version: unsafe { native_string((api.version)()) },
            decoder,
            white_balance: if camera_wb {
                RawWhiteBalance::Camera
            } else {
                RawWhiteBalance::DaylightFallback
            },
            camera_multipliers: camera_wb.then_some(multipliers),
            converted_to_srgb: true,
            orientation_applied: true,
        },
    })
}

fn sanitize_camera_multipliers(m: &mut [f32; 4]) -> bool {
    if !m[..3]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0 && *v <= 65_536.0)
    {
        return false;
    }
    // RGB cameras commonly leave the second green slot at zero.
    if m[3] == 0.0 {
        m[3] = m[1];
    }
    m[3].is_finite() && m[3] > 0.0 && m[3] <= 65_536.0
}

struct MemoryImage<'a> {
    ptr: NonNull<MemHeader>,
    api: &'a Api,
}
impl Drop for MemoryImage<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.clear_image)(self.ptr.as_ptr()) };
    }
}
fn validate_header(h: &MemHeader, info: &RawInfo) -> Result<usize> {
    let pixels = validate_dimensions(i32::from(h.width), i32::from(h.height))?;
    if h.kind != LIBRAW_IMAGE_BITMAP || !matches!(h.colors, 1 | 3) || !matches!(h.bits, 8 | 16) {
        bail!("Unsupported LibRaw sensor bitmap type/channels/bit depth");
    }
    // Reject unexpected native resizing rather than reading an unadmitted image.
    if u32::from(h.width) != info.width || u32::from(h.height) != info.height {
        bail!("LibRaw sensor bitmap dimensions changed after admission");
    }
    let count = usize::try_from(pixels * u64::from(h.colors) * u64::from(h.bits / 8))?;
    if count != h.data_size as usize || count > info.working_bytes {
        bail!("Invalid LibRaw sensor bitmap byte count");
    }
    Ok(count)
}
fn copy_pixels(h: &MemHeader, data: &[u8]) -> Result<DynamicImage> {
    let (w, hgt) = (u32::from(h.width), u32::from(h.height));
    match (h.colors, h.bits) {
        (3, 8) => Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, hgt, data.to_vec()).context("Invalid RAW RGB buffer")?,
        )),
        (1, 8) => Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, hgt, data.to_vec()).context("Invalid RAW grayscale buffer")?,
        )),
        // LibRaw's memory writer stores 16-bit samples in native byte order.
        (colors, 16) => {
            let samples: Vec<u16> = data
                .chunks_exact(2)
                .map(|b| u16::from_ne_bytes([b[0], b[1]]))
                .collect();
            match colors {
                3 => Ok(DynamicImage::ImageRgb16(
                    ImageBuffer::<Rgb<u16>, _>::from_raw(w, hgt, samples)
                        .context("Invalid RAW RGB16 buffer")?,
                )),
                1 => Ok(DynamicImage::ImageLuma16(
                    ImageBuffer::<Luma<u16>, _>::from_raw(w, hgt, samples)
                        .context("Invalid RAW grayscale16 buffer")?,
                )),
                _ => bail!("Unsupported RAW channel count"),
            }
        }
        _ => bail!("Unsupported RAW bitmap format"),
    }
}
unsafe fn native_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        "unknown".into()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_source_and_sensor_allocation_bombs() {
        assert!(validate_source_len(0).is_err());
        assert!(validate_source_len(MAX_SOURCE + 1).is_err());
        assert!(validate_source_len(MAX_SOURCE).is_ok());
        assert!(inspection_working_bytes(MAX_SOURCE + 1).is_err());
        assert!(inspection_working_bytes(8 * 1024 * 1024).unwrap() >= 16 * 1024 * 1024);
        for dimensions in [(0, 1), (-1, 1), (1, 65_536), (20_000, 20_000)] {
            assert!(make_info(dimensions.0, dimensions.1, 1, 1).is_err());
            assert!(make_info(1, 1, dimensions.0, dimensions.1).is_err());
        }
        // Sensor margins must be charged even for a tiny output crop.
        let cropped = make_info(10_000, 10_000, 100, 100).unwrap();
        assert!(cropped.working_bytes as u64 > 100_000_000 * 16);
        assert!(make_info(16_000, 10_000, 16_000, 10_000).is_ok());
    }

    #[test]
    fn rejects_native_header_confusion_and_unadmitted_resizing() {
        assert_eq!(std::mem::size_of::<MemHeader>(), 16);
        let info = make_info(64, 48, 64, 48).unwrap();
        let valid = MemHeader {
            kind: 2,
            width: 64,
            height: 48,
            colors: 3,
            bits: 8,
            data_size: 64 * 48 * 3,
        };
        assert_eq!(validate_header(&valid, &info).unwrap(), 64 * 48 * 3);
        for broken in [
            MemHeader { kind: 1, ..valid }, // JPEG is not a sensor RGB bitmap.
            MemHeader { colors: 4, ..valid },
            MemHeader { bits: 12, ..valid },
            MemHeader {
                data_size: valid.data_size - 1,
                ..valid
            },
            MemHeader {
                data_size: u32::MAX,
                ..valid
            },
            MemHeader {
                width: 48,
                height: 64,
                ..valid
            }, // double rotation.
            MemHeader { width: 0, ..valid },
        ] {
            assert!(validate_header(&broken, &info).is_err(), "{broken:?}");
        }
        let sixteen = MemHeader {
            bits: 16,
            data_size: valid.data_size * 2,
            ..valid
        };
        assert_eq!(validate_header(&sixteen, &info).unwrap(), 64 * 48 * 6);
    }

    #[test]
    fn native_endian_sixteen_bit_samples_preserve_color() {
        let header = MemHeader {
            kind: 2,
            width: 1,
            height: 1,
            colors: 3,
            bits: 16,
            data_size: 6,
        };
        let bytes: Vec<u8> = [0x1234u16, 0xabcd, 0xffff]
            .into_iter()
            .flat_map(u16::to_ne_bytes)
            .collect();
        let output = copy_pixels(&header, &bytes).unwrap().into_rgb16();
        assert_eq!(output.get_pixel(0, 0).0, [0x1234, 0xabcd, 0xffff]);
    }

    #[test]
    fn invalid_as_shot_metadata_uses_documented_daylight_fallback() {
        let mut camera = [2.0, 1.0, 1.5, 0.0];
        assert!(sanitize_camera_multipliers(&mut camera));
        assert_eq!(camera, [2.0, 1.0, 1.5, 1.0]);
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY, 1.0e20] {
            let mut metadata = [bad, 1.0, 1.5, 1.0];
            assert!(!sanitize_camera_multipliers(&mut metadata));
        }
    }

    #[test]
    fn already_cancelled_work_never_requires_a_native_library() {
        let cancelled = AtomicBool::new(true);
        let error = decode_with_cancel(&[0; 16], usize::MAX, &cancelled)
            .err()
            .unwrap();
        assert!(error.to_string().contains("cancelled"));
    }

    // Original synthetic CFA fixture, dedicated to CC0-1.0. This TIFF contains
    // one uncompressed 16-bit RGGB sensor plane, actual DNG CFA/matrix/WB tags,
    // and NO embedded JPEG, thumbnail, or pre-rendered RGB image.
    fn sensor_dng(orientation: u16) -> Vec<u8> {
        let short = |values: &[u16]| {
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>()
        };
        let long = |value: u32| value.to_le_bytes().to_vec();
        let rational = |values: &[(i32, i32)]| {
            values
                .iter()
                .flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat())
                .collect::<Vec<_>>()
        };
        let mut tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (256, 4, 1, long(64)),
            (257, 4, 1, long(48)),
            (258, 3, 1, short(&[16])),
            (259, 3, 1, short(&[1])),
            (262, 3, 1, short(&[32803])),
            (271, 2, 6, b"Wiker\0".to_vec()),
            (272, 2, 12, b"Test Sensor\0".to_vec()),
            (273, 4, 1, long(0)),
            (274, 3, 1, short(&[orientation])),
            (277, 3, 1, short(&[1])),
            (278, 4, 1, long(48)),
            (279, 4, 1, long(64 * 48 * 2)),
            (284, 3, 1, short(&[1])),
            (33421, 3, 2, short(&[2, 2])),
            (33422, 1, 4, vec![0, 1, 1, 2]),
            (50706, 1, 4, vec![1, 4, 0, 0]),
            (50707, 1, 4, vec![1, 1, 0, 0]),
            (50708, 2, 20, b"Wiker Synthetic CFA\0".to_vec()),
            (50710, 1, 3, vec![0, 1, 2]),
            (50711, 3, 1, short(&[1])),
            (50714, 5, 1, rational(&[(64, 1)])),
            (50717, 4, 1, long(4095)),
            (
                50721,
                10,
                9,
                rational(&[
                    (3240454, 1000000),
                    (-1537139, 1000000),
                    (-498531, 1000000),
                    (-969266, 1000000),
                    (1876011, 1000000),
                    (41556, 1000000),
                    (55643, 1000000),
                    (-204026, 1000000),
                    (1057225, 1000000),
                ]),
            ),
            (50728, 5, 3, rational(&[(1, 2), (1, 1), (2, 3)])),
            (50778, 3, 1, short(&[21])),
        ];
        tags.sort_by_key(|t| t.0);
        let start = 8 + 2 + 12 * tags.len() + 4;
        let mut extra = Vec::new();
        let mut entries = Vec::new();
        for (tag, kind, count, mut payload) in tags {
            let value = if payload.len() <= 4 {
                payload.resize(4, 0);
                payload
            } else {
                let address = long((start + extra.len()) as u32);
                extra.extend(payload);
                if extra.len() % 2 != 0 {
                    extra.push(0);
                }
                address
            };
            entries.push((tag, kind, count, value));
        }
        let pixel_start = start + extra.len();
        let mut out = b"II*\0".to_vec();
        out.extend(long(8));
        out.extend(short(&[entries.len() as u16]));
        for (tag, kind, count, value) in entries {
            out.extend(short(&[tag, kind]));
            out.extend(long(count));
            out.extend(if tag == 273 {
                long(pixel_start as u32)
            } else {
                value
            });
        }
        out.extend(long(0));
        out.extend(extra);
        let quadrants = [
            [1600u16, 180, 240],
            [90, 3200, 120],
            [120, 240, 2100],
            [1400, 2800, 1867],
        ];
        for y in 0..48 {
            for x in 0..64 {
                let rgb = quadrants[usize::from(y >= 24) * 2 + usize::from(x >= 32)];
                let channel = [[0, 1], [1, 2]][y % 2][x % 2];
                out.extend(short(&[64 + rgb[channel]]));
            }
        }
        out
    }

    #[test]
    #[ignore = "requires a compatible native LibRaw; renders actual CFA sensor data"]
    fn native_sensor_dng_has_color_and_applies_all_eight_orientations_once() {
        use image::metadata::Orientation;
        let bytes = sensor_dng(1);
        let info = inspect(&bytes).unwrap();
        assert_eq!(
            (info.raw_width, info.raw_height, info.width, info.height),
            (64, 48, 64, 48)
        );
        let rendered = decode(&bytes, info.working_bytes).unwrap();
        assert_eq!(rendered.provenance.white_balance, RawWhiteBalance::Camera);
        assert_eq!(
            rendered.provenance.camera_multipliers.unwrap()[..3],
            [2.0, 1.0, 1.5]
        );
        assert!(rendered.provenance.orientation_applied && rendered.provenance.converted_to_srgb);
        let rgb = rendered.image.to_rgb8();
        for (x, y, channel) in [(16, 12, 0), (48, 12, 1), (16, 36, 2)] {
            let p = rgb.get_pixel(x, y).0;
            assert!(
                p[channel] > 150
                    && p[channel] > p[(channel + 1) % 3].saturating_add(60)
                    && p[channel] > p[(channel + 2) % 3].saturating_add(60),
                "RGB patch {x},{y}: {p:?}"
            );
        }
        for exif in 2..=8 {
            let mut expected = rendered.image.clone();
            expected.apply_orientation(Orientation::from_exif(exif).unwrap());
            let bytes = sensor_dng(exif as u16);
            let info = inspect(&bytes).unwrap();
            let output = decode(&bytes, info.working_bytes).unwrap();
            assert_eq!(output.image.to_rgb8(), expected.to_rgb8(), "EXIF {exif}");
            assert_eq!(
                (output.source_width, output.source_height),
                (info.width, info.height)
            );
        }
    }

    #[test]
    #[ignore = "requires native LibRaw; checks admission before sensor unpack and invalid headers"]
    fn native_sensor_admission_and_malformed_input_are_bounded() {
        let bytes = sensor_dng(1);
        let info = inspect(&bytes).unwrap();
        for budget in [0, info.working_bytes - 1] {
            let error = decode(&bytes, budget).err().unwrap().to_string();
            assert!(error.contains("admitted"), "{error}");
        }
        // Unknown as-shot WB must remain renderable via the documented
        // daylight fallback, rather than installing zero/invalid multipliers.
        let mut no_wb = sensor_dng(1);
        let count = u16::from_le_bytes([no_wb[8], no_wb[9]]) as usize;
        for index in 0..count {
            let at = 10 + 12 * index;
            if u16::from_le_bytes([no_wb[at], no_wb[at + 1]]) == 50728 {
                no_wb[at..at + 2].copy_from_slice(&65000u16.to_le_bytes());
            }
        }
        let rendered = decode(&no_wb, inspect(&no_wb).unwrap().working_bytes).unwrap();
        assert_eq!(
            rendered.provenance.white_balance,
            RawWhiteBalance::DaylightFallback
        );
        assert!(rendered.provenance.camera_multipliers.is_none());
        for len in [0, 1, 8, 16, 64, 256, 1024] {
            assert!(inspect(&vec![0u8; len]).is_err());
        }
        // A valid-looking IFD declares an oversized sensor, with no large
        // allocation needed to construct the adversarial fixture.
        let mut huge = sensor_dng(1);
        let count = u16::from_le_bytes([huge[8], huge[9]]) as usize;
        for index in 0..count {
            let at = 10 + 12 * index;
            let tag = u16::from_le_bytes([huge[at], huge[at + 1]]);
            if tag == 256 || tag == 257 {
                huge[at + 8..at + 12].copy_from_slice(&20_000u32.to_le_bytes());
            }
        }
        assert!(inspect(&huge).is_err());
    }

    #[test]
    #[ignore = "set SHADOWROOM_RAW_FIXTURE to a real camera RAW; requires native LibRaw"]
    fn native_real_camera_sensor_render() {
        let path = std::env::var_os("SHADOWROOM_RAW_FIXTURE").expect("set SHADOWROOM_RAW_FIXTURE");
        let bytes = std::fs::read(path).unwrap();
        let info = inspect(&bytes).unwrap();
        let output = decode(&bytes, info.working_bytes).unwrap();
        assert_eq!(
            (output.image.width(), output.image.height()),
            (info.width, info.height)
        );
        assert!(output.provenance.converted_to_srgb && output.provenance.orientation_applied);
        assert!(!output.provenance.decoder.is_empty());
        let rgb = output.image.to_rgb8();
        let first = rgb.as_raw()[0];
        assert!(
            rgb.as_raw().iter().any(|v| v.abs_diff(first) > 16),
            "uniform/empty sensor render"
        );
        eprintln!(
            "LibRaw {} / {}: {}x{}, WB {:?}, {} admitted bytes",
            output.provenance.libraw_version,
            output.provenance.decoder,
            info.width,
            info.height,
            output.provenance.white_balance,
            info.working_bytes
        );
    }
}
