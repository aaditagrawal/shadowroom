//! Bounded HEIF header inspection and read-only SDR decoding.
//! Pixel allocation is reachable only after the caller admits working_bytes.
use anyhow::{Context, Result, bail};
use image::DynamicImage;
#[cfg(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(feature = "native-codecs", feature = "rust-heif")
    )
))]
use image::RgbaImage;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const MAX_META: usize = 8 * 1024 * 1024;
const MAX_PROFILE: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u64 = 160_000_000;
const MAX_BOXES: usize = 4096;
#[cfg(all(target_os = "linux", feature = "rust-heif"))]
#[path = "codecs/rust_heif.rs"]
mod rust_backend;
// Match libheif's 256×256 coded-surface floor. Tiny HEIF display images
// can use larger hardware-encoder surfaces (e.g. 2×2 stored in 160×64).
#[cfg(target_os = "linux")]
const MIN_CODED_EDGE: u32 = 256;

#[derive(Clone, Debug)]
pub struct HeifInfo {
    /// Display dimensions after container rotation and clean aperture.
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    /// Largest declared coded image/tile, including Rust HEVC block padding
    /// when that backend is enabled, used for conservative admission.
    pub allocation_pixels: u64,
    pub bit_depth: u8,
    pub color_primaries: Option<u16>,
    pub transfer_characteristics: Option<u16>,
    pub has_icc: bool,
}

pub struct DecodedHeif {
    pub image: DynamicImage,
    pub icc: Option<Vec<u8>>,
    pub converted_to_srgb: bool,
    pub source_width: u32,
    pub source_height: u32,
}

pub fn is_heif(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        ["heic", "heif", "hif"]
            .iter()
            .any(|e| s.eq_ignore_ascii_case(e))
    })
}

#[derive(Clone, Copy)]
struct BoxRef<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}
fn boxes(mut data: &[u8]) -> Result<Vec<BoxRef<'_>>> {
    let mut result = Vec::new();
    while !data.is_empty() {
        if result.len() == MAX_BOXES {
            bail!("HEIF box count exceeds safety limit");
        }
        if data.len() < 8 {
            bail!("Truncated HEIF box header");
        }
        let kind = data[4..8].try_into().unwrap();
        let short = u32::from_be_bytes(data[..4].try_into().unwrap());
        let (size, header) = match short {
            0 => (data.len(), 8),
            1 => {
                let extended = data.get(8..16).context("Truncated extended HEIF box")?;
                (
                    usize::try_from(u64::from_be_bytes(extended.try_into().unwrap()))?,
                    16,
                )
            }
            n => (n as usize, 8),
        };
        if size < header || size > data.len() {
            bail!("Invalid HEIF box size");
        }
        result.push(BoxRef {
            kind,
            data: &data[header..size],
        });
        data = &data[size..];
    }
    Ok(result)
}
fn unique_box<'a>(entries: &[BoxRef<'a>], kind: [u8; 4]) -> Result<BoxRef<'a>> {
    let mut matching = entries.iter().filter(|entry| entry.kind == kind);
    let first = *matching
        .next()
        .context("Required HEIF metadata box missing")?;
    if matching.next().is_some() {
        bail!("Duplicate HEIF metadata boxes are unsupported");
    }
    Ok(first)
}
fn be16(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        data.get(offset..offset + 2)
            .context("Truncated HEIF field")?
            .try_into()
            .unwrap(),
    ))
}
fn be32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        data.get(offset..offset + 4)
            .context("Truncated HEIF field")?
            .try_into()
            .unwrap(),
    ))
}
fn validate_size(width: u32, height: u32) -> Result<()> {
    if width == 0
        || height == 0
        || width > 100_000
        || height > 100_000
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        bail!("HEIF dimensions exceed the 160 megapixel safety limit");
    }
    Ok(())
}

/// Parse only bounded container metadata. No native library or pixel decoder runs.
pub fn inspect(bytes: &[u8]) -> Result<HeifInfo> {
    let top = boxes(bytes)?;
    let ftyp = unique_box(&top, *b"ftyp")?;
    if ftyp.data.len() < 8
        || ftyp.data.len() > 1024
        || !ftyp.data.chunks_exact(4).enumerate().any(|(i, brand)| {
            i != 1
                && matches!(
                    brand,
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
                )
        })
    {
        bail!("Unsupported HEIF file brand");
    }
    let meta = unique_box(&top, *b"meta")?;
    if meta.data.len() > MAX_META {
        bail!("HEIF metadata exceeds 8 MiB safety limit");
    }
    let children = boxes(meta.data.get(4..).context("Truncated HEIF metadata")?)?;
    let pitm = unique_box(&children, *b"pitm")?;
    let primary = match pitm.data.first() {
        Some(0) => u32::from(be16(pitm.data, 4)?),
        Some(1) => be32(pitm.data, 4)?,
        _ => bail!("Unsupported HEIF primary item version"),
    };
    let iprp = unique_box(&children, *b"iprp")?;
    let iprp = boxes(iprp.data)?;
    let ipco = unique_box(&iprp, *b"ipco")?;
    let properties = boxes(ipco.data)?;
    if properties.len() > 1024 {
        bail!("HEIF property count exceeds safety limit");
    }
    // Validate every coded tile's dimensions, not just the canvas. Malformed
    // hidden/tiled images cannot smuggle an oversized native allocation.
    let mut allocation_pixels = 0;
    for property in &properties {
        if property.kind == *b"ispe" {
            let (w, h) = (be32(property.data, 4)?, be32(property.data, 8)?);
            validate_size(w, h)?;
            let pixels = u64::from(w) * u64::from(h);
            // ispe describes the cropped surface, while HEVC's SPS can encode
            // padding up to the next coding block. Charge those padded pixels
            // before permitting the optional Rust decoder to allocate them.
            #[cfg(all(target_os = "linux", feature = "rust-heif"))]
            let pixels =
                pixels.max(u64::from(w.div_ceil(64) * 64) * u64::from(h.div_ceil(64) * 64));
            allocation_pixels = allocation_pixels.max(pixels);
        }
    }
    let mut associated = Vec::new();
    for ipma in iprp.iter().filter(|b| b.kind == *b"ipma") {
        let version = *ipma.data.first().context("Truncated HEIF associations")?;
        if version > 1 {
            bail!("Unsupported HEIF association version");
        }
        let wide = be32(ipma.data, 0)? & 1 != 0;
        let count = be32(ipma.data, 4)? as usize;
        if count > MAX_BOXES {
            bail!("HEIF item count exceeds safety limit");
        }
        let mut offset = 8;
        for _ in 0..count {
            let id = if version == 0 {
                let v = u32::from(be16(ipma.data, offset)?);
                offset += 2;
                v
            } else {
                let v = be32(ipma.data, offset)?;
                offset += 4;
                v
            };
            let n = *ipma
                .data
                .get(offset)
                .context("Truncated HEIF association count")? as usize;
            offset += 1;
            for _ in 0..n {
                let property = if wide {
                    let v = be16(ipma.data, offset)? & 0x7fff;
                    offset += 2;
                    v
                } else {
                    let v = *ipma
                        .data
                        .get(offset)
                        .context("Truncated HEIF association")?
                        & 0x7f;
                    offset += 1;
                    u16::from(v)
                };
                if property != 0 && property as usize > properties.len() {
                    bail!("Invalid HEIF property association");
                }
                if id == primary && property != 0 {
                    if associated.len() >= 64 {
                        bail!("HEIF primary property count exceeds safety limit");
                    }
                    associated.push(properties[property as usize - 1]);
                }
            }
        }
    }
    let mut info = HeifInfo {
        width: 0,
        height: 0,
        coded_width: 0,
        coded_height: 0,
        allocation_pixels,
        bit_depth: 8,
        color_primaries: None,
        transfer_characteristics: None,
        has_icc: false,
    };
    let mut rotation = 0;
    let mut aperture = None;
    for property in associated {
        match &property.kind {
            b"ispe" => {
                info.coded_width = be32(property.data, 4)?;
                info.coded_height = be32(property.data, 8)?;
            }
            b"irot" => {
                rotation = *property.data.first().context("Truncated HEIF rotation")? & 3;
            }
            b"clap" => {
                let (wn, wd, hn, hd) = (
                    be32(property.data, 0)?,
                    be32(property.data, 4)?,
                    be32(property.data, 8)?,
                    be32(property.data, 12)?,
                );
                if wd == 0 || hd == 0 || wn % wd != 0 || hn % hd != 0 {
                    bail!("Unsupported fractional HEIF clean aperture");
                }
                aperture = Some((wn / wd, hn / hd));
            }
            b"pixi" => {
                let count = *property.data.get(4).context("Truncated HEIF pixel depth")? as usize;
                if count == 0 || count > 4 {
                    bail!("Unsupported HEIF channel count");
                }
                info.bit_depth = info.bit_depth.max(
                    *property
                        .data
                        .get(5..5 + count)
                        .context("Truncated HEIF channel depths")?
                        .iter()
                        .max()
                        .unwrap(),
                );
            }
            b"hvcC" => {
                info.bit_depth = info.bit_depth.max(
                    8 + (*property
                        .data
                        .get(17)
                        .context("Truncated HEIF HEVC configuration")?
                        & 7),
                );
                info.bit_depth = info.bit_depth.max(
                    8 + (*property
                        .data
                        .get(18)
                        .context("Truncated HEIF HEVC configuration")?
                        & 7),
                );
            }
            b"colr" if property.data.starts_with(b"nclx") => {
                info.color_primaries = Some(be16(property.data, 4)?);
                info.transfer_characteristics = Some(be16(property.data, 6)?);
            }
            b"colr" if property.data.starts_with(b"prof") || property.data.starts_with(b"rICC") => {
                if property.data.len() - 4 > MAX_PROFILE {
                    bail!("HEIF ICC profile exceeds 4 MiB safety limit");
                }
                info.has_icc = true;
            }
            _ => {}
        }
    }
    validate_size(info.coded_width, info.coded_height)?;
    let (width, height) = aperture.unwrap_or((info.coded_width, info.coded_height));
    validate_size(width, height)?;
    if width > info.coded_width || height > info.coded_height {
        bail!("HEIF aperture exceeds coded dimensions");
    }
    (info.width, info.height) = if rotation % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    Ok(info)
}

/// Seek over compressed payloads while reading at most one 8 MiB metadata box.
pub fn inspect_file(path: &Path) -> Result<HeifInfo> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut offset = 0u64;
    let mut metadata = Vec::new();
    let mut found_meta = false;
    let mut found_ftyp = false;
    for _ in 0..MAX_BOXES {
        if offset == length {
            break;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;
        let short = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, head) = match short {
            0 => (length - offset, 8),
            1 => {
                file.read_exact(&mut header[8..])?;
                (u64::from_be_bytes(header[8..].try_into().unwrap()), 16)
            }
            n => (u64::from(n), 8),
        };
        if size < head as u64 || size > length - offset {
            bail!("Invalid HEIF file box size");
        }
        let kind = &header[4..8];
        if kind == b"ftyp" || kind == b"meta" {
            if (kind == b"ftyp" && found_ftyp) || (kind == b"meta" && found_meta) {
                bail!("Duplicate HEIF metadata/type boxes are unsupported");
            }
            let maximum = if kind == b"ftyp" { 1024 } else { MAX_META + 16 };
            if size > maximum as u64 {
                bail!("HEIF header exceeds metadata safety limit");
            }
            if metadata.len().saturating_add(size as usize) > MAX_META + 2048 {
                bail!("HEIF combined metadata exceeds safety limit");
            }
            metadata.extend_from_slice(&header[..head]);
            let previous = metadata.len();
            metadata.resize(previous + size as usize - head, 0);
            file.read_exact(&mut metadata[previous..])?;
            if kind == b"meta" {
                found_meta = true;
            } else {
                found_ftyp = true;
            }
        }
        offset = offset.checked_add(size).context("HEIF offset overflow")?;
    }
    if offset != length {
        bail!("HEIF file box count exceeds safety limit");
    }
    inspect(&metadata)
}

fn validate_sdr(info: &HeifInfo) -> Result<()> {
    if info.bit_depth > 16 || matches!(info.transfer_characteristics, Some(16 | 18)) {
        bail!("HEIF HDR transfer or unsupported sample depth; use an SDR companion");
    }
    if !info.has_icc
        && (info
            .color_primaries
            .is_some_and(|p| !matches!(p, 1 | 2 | 9 | 12))
            || info
                .transfer_characteristics
                .is_some_and(|t| !matches!(t, 1 | 2 | 6 | 8 | 13 | 14 | 15)))
    {
        bail!("HEIF NCLX color encoding is unsupported without an embedded ICC profile");
    }
    Ok(())
}

pub fn working_bytes(bytes: &[u8], max_edge: Option<u32>) -> Result<usize> {
    let info = inspect(bytes)?;
    validate_sdr(&info)?;
    let pixels = info.allocation_pixels;
    #[cfg(target_os = "linux")]
    let pixels = pixels.max(u64::from(MIN_CODED_EDGE).pow(2));
    #[cfg(target_os = "macos")]
    let pixel_workspace = {
        let ratio = max_edge
            .map(|e| (f64::from(e) / f64::from(info.width.max(info.height))).min(1.0))
            .unwrap_or(1.0);
        // Native thumbnailing avoids our full RGB copy; charge source codec
        // workspace as well as the scaled output/color/resampling buffers.
        pixels
            .saturating_mul(20)
            .saturating_add((pixels as f64 * ratio * ratio).ceil() as u64 * 20)
    };
    #[cfg(not(target_os = "macos"))]
    let pixel_workspace = {
        let _ = max_edge;
        pixels.saturating_mul(40)
    };
    usize::try_from(
        pixel_workspace
            .saturating_add(bytes.len() as u64 * 2)
            .saturating_add(MAX_META as u64 + MAX_PROFILE as u64 + 1024 * 1024),
    )
    .context("HEIF working memory exceeds address space")
}

pub fn decode(bytes: &[u8], max_edge: Option<u32>, admitted_bytes: usize) -> Result<DecodedHeif> {
    let required = working_bytes(bytes, max_edge)?;
    if required > admitted_bytes {
        bail!(
            "HEIF working memory requires approximately {} MiB; increase memory budget",
            required.div_ceil(1024 * 1024)
        );
    }
    let info = inspect(bytes)?;
    #[cfg(all(target_os = "linux", feature = "rust-heif"))]
    if let Some(decoded) = rust_backend::decode(bytes, &info)? {
        return Ok(decoded);
    }
    platform::decode(bytes, max_edge, &info)
}

#[cfg(all(
    target_os = "linux",
    any(feature = "native-codecs", feature = "rust-heif")
))]
mod sdr_color {
    use super::*;
    pub(super) trait SdrSample: image::Primitive {
        fn normalized(self) -> f64;
        fn from_normalized(value: f64) -> Self;
    }
    impl SdrSample for u8 {
        fn normalized(self) -> f64 {
            f64::from(self) / 255.
        }
        fn from_normalized(value: f64) -> Self {
            (value * 255.).round() as u8
        }
    }
    impl SdrSample for u16 {
        fn normalized(self) -> f64 {
            f64::from(self) / 65535.
        }
        fn from_normalized(value: f64) -> Self {
            (value * 65535.).round() as u16
        }
    }
    pub(super) fn convert_nclx<T: SdrSample>(
        image: &mut image::ImageBuffer<image::Rgba<T>, Vec<T>>,
        info: &HeifInfo,
    ) -> Result<bool>
    where
        image::Rgba<T>: image::Pixel<Subpixel = T>,
    {
        let (Some(primaries), Some(transfer)) =
            (info.color_primaries, info.transfer_characteristics)
        else {
            return Ok(false);
        };
        if primaries == 2 || transfer == 2 {
            return Ok(false);
        }
        if primaries == 1 && transfer == 13 {
            return Ok(true);
        }
        for pixel in image.pixels_mut() {
            let mut rgb = [0.; 3];
            for (out, input) in rgb.iter_mut().zip(pixel.0) {
                let encoded = input.normalized();
                *out = match transfer {
                    13 => {
                        if encoded <= 0.04045 {
                            encoded / 12.92
                        } else {
                            ((encoded + 0.055) / 1.055).powf(2.4)
                        }
                    }
                    8 => encoded,
                    1 | 6 | 14 | 15 => {
                        if encoded < 0.081 {
                            encoded / 4.5
                        } else {
                            ((encoded + 0.099) / 1.099).powf(1. / 0.45)
                        }
                    }
                    _ => bail!("Unsupported HEIF SDR transfer"),
                };
            }
            let matrix = match primaries {
                1 => [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                12 => [
                    [1.224745, -0.224904, 0.],
                    [-0.042058, 1.042081, 0.],
                    [-0.019642, -0.078655, 1.098537],
                ],
                9 => [
                    [1.660491, -0.587641, -0.072850],
                    [-0.124550, 1.132900, -0.008350],
                    [-0.018151, -0.100579, 1.118730],
                ],
                _ => bail!("Unsupported HEIF SDR primaries"),
            };
            for (channel, row) in pixel.0[..3].iter_mut().zip(matrix) {
                let linear = (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).clamp(0., 1.);
                let encoded = if linear <= 0.0031308 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1. / 2.4) - 0.055
                };
                *channel = T::from_normalized(encoded);
            }
        }
        Ok(true)
    }
}

#[cfg(all(target_os = "linux", feature = "native-codecs"))]
mod platform {
    use super::sdr_color::convert_nclx;
    use super::*;
    use libloading::Library;
    use std::{
        ffi::{CStr, c_char, c_int, c_void},
        ptr,
        sync::OnceLock,
    };
    #[repr(C)]
    struct HeifError {
        code: c_int,
        subcode: c_int,
        message: *const c_char,
    }
    impl HeifError {
        fn check(self) -> Result<()> {
            if self.code == 0 {
                return Ok(());
            }
            let message = if self.message.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(self.message) }
                    .to_string_lossy()
                    .chars()
                    .take(512)
                    .collect()
            };
            bail!(
                "Native HEIF decoder error {}/{}: {}",
                self.code,
                self.subcode,
                message
            )
        }
    }
    struct Api {
        context_alloc: unsafe extern "C" fn() -> *mut c_void,
        context_free: unsafe extern "C" fn(*mut c_void),
        context_read:
            unsafe extern "C" fn(*mut c_void, *const c_void, usize, *const c_void) -> HeifError,
        set_size: unsafe extern "C" fn(*mut c_void, c_int),
        set_threads: unsafe extern "C" fn(*mut c_void, c_int),
        primary: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> HeifError,
        handle_free: unsafe extern "C" fn(*mut c_void),
        width: unsafe extern "C" fn(*mut c_void) -> c_int,
        height: unsafe extern "C" fn(*mut c_void) -> c_int,
        luma_bits: unsafe extern "C" fn(*mut c_void) -> c_int,
        chroma_bits: unsafe extern "C" fn(*mut c_void) -> c_int,
        premultiplied_alpha: unsafe extern "C" fn(*mut c_void) -> c_int,
        profile_size: unsafe extern "C" fn(*mut c_void) -> usize,
        profile: unsafe extern "C" fn(*mut c_void, *mut c_void) -> HeifError,
        decode_image: unsafe extern "C" fn(
            *mut c_void,
            *mut *mut c_void,
            c_int,
            c_int,
            *const c_void,
        ) -> HeifError,
        image_free: unsafe extern "C" fn(*mut c_void),
        image_width: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
        image_height: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
        image_bits: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
        plane: unsafe extern "C" fn(*mut c_void, c_int, *mut c_int) -> *const u8,
        _library: Library,
    }
    impl Api {
        unsafe fn load() -> Result<Self> {
            let library = unsafe { Library::new("libheif.so.1") }.context(
                "HEIF preview unavailable: install a system libheif runtime and HEVC decoder",
            )?;
            macro_rules! symbol {
                ($name:literal) => {
                    unsafe { *library.get(concat!($name, "\0").as_bytes())? }
                };
            }
            if let Ok(init) = unsafe {
                library.get::<unsafe extern "C" fn(*const c_void) -> HeifError>(b"heif_init\0")
            } {
                unsafe { init(ptr::null()) }.check()?;
            }
            Ok(Self {
                context_alloc: symbol!("heif_context_alloc"),
                context_free: symbol!("heif_context_free"),
                context_read: symbol!("heif_context_read_from_memory_without_copy"),
                set_size: symbol!("heif_context_set_maximum_image_size_limit"),
                set_threads: symbol!("heif_context_set_max_decoding_threads"),
                primary: symbol!("heif_context_get_primary_image_handle"),
                handle_free: symbol!("heif_image_handle_release"),
                width: symbol!("heif_image_handle_get_width"),
                height: symbol!("heif_image_handle_get_height"),
                luma_bits: symbol!("heif_image_handle_get_luma_bits_per_pixel"),
                chroma_bits: symbol!("heif_image_handle_get_chroma_bits_per_pixel"),
                premultiplied_alpha: symbol!("heif_image_handle_is_premultiplied_alpha"),
                profile_size: symbol!("heif_image_handle_get_raw_color_profile_size"),
                profile: symbol!("heif_image_handle_get_raw_color_profile"),
                decode_image: symbol!("heif_decode_image"),
                image_free: symbol!("heif_image_release"),
                image_width: symbol!("heif_image_get_width"),
                image_height: symbol!("heif_image_get_height"),
                image_bits: symbol!("heif_image_get_bits_per_pixel_range"),
                plane: symbol!("heif_image_get_plane_readonly"),
                _library: library,
            })
        }
    }
    fn api() -> Result<&'static Api> {
        static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
        API.get_or_init(|| unsafe { Api::load() }.map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(|s| anyhow::anyhow!("{s}"))
    }
    struct Owned {
        pointer: *mut c_void,
        release: unsafe extern "C" fn(*mut c_void),
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                unsafe { (self.release)(self.pointer) }
            }
        }
    }
    pub fn decode(bytes: &[u8], _max_edge: Option<u32>, info: &HeifInfo) -> Result<DecodedHeif> {
        let api = api()?;
        unsafe {
            let context = Owned {
                pointer: (api.context_alloc)(),
                release: api.context_free,
            };
            if context.pointer.is_null() {
                bail!("Allocate native HEIF context failed");
            }
            (api.set_size)(
                context.pointer,
                info.coded_width.max(info.coded_height).max(MIN_CODED_EDGE) as c_int,
            );
            (api.set_threads)(context.pointer, 0);
            (api.context_read)(
                context.pointer,
                bytes.as_ptr().cast(),
                bytes.len(),
                ptr::null(),
            )
            .check()?;
            let mut handle = Owned {
                pointer: ptr::null_mut(),
                release: api.handle_free,
            };
            (api.primary)(context.pointer, &mut handle.pointer).check()?;
            if handle.pointer.is_null() {
                bail!("Native HEIF primary image missing");
            }
            if (api.premultiplied_alpha)(handle.pointer) != 0 {
                bail!("Premultiplied-alpha HEIF is unsupported; use straight alpha");
            }
            let width = u32::try_from((api.width)(handle.pointer))?;
            let height = u32::try_from((api.height)(handle.pointer))?;
            validate_size(width, height)?;
            if u64::from(width) * u64::from(height)
                > u64::from(info.coded_width) * u64::from(info.coded_height)
            {
                bail!("Native HEIF dimensions exceed admitted header dimensions");
            }
            if (api.luma_bits)(handle.pointer) > 16 || (api.chroma_bits)(handle.pointer) > 16 {
                bail!("Native HEIF sample depth exceeds 16 bits");
            }
            let profile_size = (api.profile_size)(handle.pointer);
            if profile_size > MAX_PROFILE {
                bail!("HEIF ICC profile exceeds safety limit");
            }
            let icc = if profile_size > 0 {
                let mut p = vec![0; profile_size];
                (api.profile)(handle.pointer, p.as_mut_ptr().cast()).check()?;
                Some(p)
            } else {
                None
            };
            let mut decoded = Owned {
                pointer: ptr::null_mut(),
                release: api.image_free,
            };
            // RGB=1, interleaved RGBA=11. Defaults apply container transformations
            // exactly once; EXIF orientation is not applied again by media.rs.
            let high_depth = info.bit_depth > 8;
            // RGBA16 little endian keeps SDR source precision through color
            // conversion; quantize only once when making the display preview.
            (api.decode_image)(
                handle.pointer,
                &mut decoded.pointer,
                1,
                if high_depth { 15 } else { 11 },
                ptr::null(),
            )
            .check()?;
            if decoded.pointer.is_null() {
                bail!("Native HEIF decoder returned no pixels");
            }
            let w = u32::try_from((api.image_width)(decoded.pointer, 10))?;
            let h = u32::try_from((api.image_height)(decoded.pointer, 10))?;
            if (w, h) != (width, height) {
                bail!("Native HEIF output dimensions differ from admitted image handle");
            }
            let mut stride = 0;
            let plane = (api.plane)(decoded.pointer, 10, &mut stride);
            let row = width as usize * if high_depth { 8 } else { 4 };
            if plane.is_null() || stride < 0 || (stride as usize) < row {
                bail!("Invalid native HEIF RGBA plane");
            }
            let (image, converted_to_srgb) = if high_depth {
                let bits = (api.image_bits)(decoded.pointer, 10);
                if !(9..=16).contains(&bits) {
                    bail!("Invalid native HEIF output depth");
                }
                let maximum = (1u32 << bits) - 1;
                let mut pixels = vec![0u16; width as usize * height as usize * 4];
                for y in 0..height as usize {
                    let input = std::slice::from_raw_parts(plane.add(y * stride as usize), row);
                    for (out, input) in pixels[y * width as usize * 4..(y + 1) * width as usize * 4]
                        .iter_mut()
                        .zip(input.chunks_exact(2))
                    {
                        let value = u32::from(u16::from_le_bytes([input[0], input[1]]));
                        if value > maximum {
                            bail!("Native HEIF sample exceeds output depth");
                        }
                        *out = ((value * 65535 + maximum / 2) / maximum) as u16;
                    }
                }
                let mut image =
                    image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(width, height, pixels)
                        .context("Invalid native HEIF pixel dimensions")?;
                let converted = if icc.is_none() {
                    convert_nclx(&mut image, info)?
                } else {
                    false
                };
                (DynamicImage::ImageRgba16(image), converted)
            } else {
                let mut pixels = vec![0; width as usize * height as usize * 4];
                for y in 0..height as usize {
                    pixels[y * row..(y + 1) * row].copy_from_slice(std::slice::from_raw_parts(
                        plane.add(y * stride as usize),
                        row,
                    ));
                }
                let mut image = RgbaImage::from_raw(width, height, pixels)
                    .context("Invalid native HEIF pixel dimensions")?;
                let converted = if icc.is_none() {
                    convert_nclx(&mut image, info)?
                } else {
                    false
                };
                (DynamicImage::ImageRgba8(image), converted)
            };
            Ok(DecodedHeif {
                image,
                icc,
                converted_to_srgb,
                source_width: width,
                source_height: height,
            })
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "native-codecs")))]
mod platform {
    use super::*;

    pub fn decode(_bytes: &[u8], _max_edge: Option<u32>, _info: &HeifInfo) -> Result<DecodedHeif> {
        bail!(
            "HEIF image requires a codec unavailable in this build: enable native-codecs for full libheif support, or rust-heif for supported SDR HEVC images"
        )
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::{
        ffi::{c_int, c_void},
        ptr,
    };
    type Ref = *const c_void;
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataCreateWithBytesNoCopy(
            allocator: Ref,
            bytes: *const u8,
            length: isize,
            bytes_allocator: Ref,
        ) -> Ref;
        fn CFDictionaryCreate(
            allocator: Ref,
            keys: *const Ref,
            values: *const Ref,
            count: isize,
            key_callbacks: Ref,
            value_callbacks: Ref,
        ) -> Ref;
        fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
        fn CFNumberCreate(allocator: Ref, kind: c_int, value: *const c_void) -> Ref;
        fn CFNumberGetValue(number: Ref, kind: c_int, value: *mut c_void) -> u8;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFRelease(value: Ref);
        static kCFAllocatorNull: Ref;
        static kCFBooleanTrue: Ref;
        static kCFBooleanFalse: Ref;
    }
    #[link(name = "ImageIO", kind = "framework")]
    unsafe extern "C" {
        fn CGImageSourceCreateWithData(data: Ref, options: Ref) -> Ref;
        fn CGImageSourceCreateThumbnailAtIndex(source: Ref, index: usize, options: Ref) -> Ref;
        fn CGImageSourceCopyPropertiesAtIndex(source: Ref, index: usize, options: Ref) -> Ref;
        static kCGImageSourceShouldCache: Ref;
        static kCGImageSourceCreateThumbnailFromImageAlways: Ref;
        static kCGImageSourceCreateThumbnailWithTransform: Ref;
        static kCGImageSourceThumbnailMaxPixelSize: Ref;
        static kCGImagePropertyPixelWidth: Ref;
        static kCGImagePropertyPixelHeight: Ref;
        static kCGImagePropertyOrientation: Ref;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGImageGetWidth(image: Ref) -> usize;
        fn CGImageGetHeight(image: Ref) -> usize;
        fn CGColorSpaceCreateWithName(name: Ref) -> Ref;
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits: usize,
            row_bytes: usize,
            space: Ref,
            flags: u32,
        ) -> Ref;
        fn CGContextDrawImage(context: Ref, rect: Rect, image: Ref);
        static kCGColorSpaceSRGB: Ref;
    }
    struct Owned(Ref);
    impl Owned {
        fn checked(pointer: Ref, message: &str) -> Result<Self> {
            if pointer.is_null() {
                bail!("{message}");
            }
            Ok(Self(pointer))
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }
    unsafe fn dictionary(keys: &[Ref], values: &[Ref]) -> Result<Owned> {
        Owned::checked(
            unsafe {
                CFDictionaryCreate(
                    ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    keys.len() as isize,
                    ptr::null(),
                    ptr::null(),
                )
            },
            "Allocate ImageIO options failed",
        )
    }
    unsafe fn property(dictionary: Ref, key: Ref) -> Option<u32> {
        let number = unsafe { CFDictionaryGetValue(dictionary, key) };
        if number.is_null() || unsafe { CFGetTypeID(number) != CFNumberGetTypeID() } {
            return None;
        }
        let mut value = 0i64;
        if unsafe { CFNumberGetValue(number, 4, (&mut value as *mut i64).cast()) } == 0 {
            return None;
        }
        u32::try_from(value).ok()
    }
    pub fn decode(bytes: &[u8], max_edge: Option<u32>, info: &HeifInfo) -> Result<DecodedHeif> {
        unsafe {
            // Data borrows the pipeline's charged source buffer for this scope.
            let data = Owned::checked(
                CFDataCreateWithBytesNoCopy(
                    ptr::null(),
                    bytes.as_ptr(),
                    isize::try_from(bytes.len())?,
                    kCFAllocatorNull,
                ),
                "Create HEIF source data failed",
            )?;
            let source_options = dictionary(&[kCGImageSourceShouldCache], &[kCFBooleanFalse])?;
            let source = Owned::checked(
                CGImageSourceCreateWithData(data.0, source_options.0),
                "ImageIO HEIF codec unavailable or invalid container",
            )?;
            let properties = Owned::checked(
                CGImageSourceCopyPropertiesAtIndex(source.0, 0, source_options.0),
                "ImageIO HEIF metadata unavailable",
            )?;
            let (mut source_width, mut source_height) = (
                property(properties.0, kCGImagePropertyPixelWidth).unwrap_or(info.width),
                property(properties.0, kCGImagePropertyPixelHeight).unwrap_or(info.height),
            );
            if matches!(
                property(properties.0, kCGImagePropertyOrientation),
                Some(5..=8)
            ) {
                std::mem::swap(&mut source_width, &mut source_height);
            }
            validate_size(source_width, source_height)?;
            if u64::from(source_width) * u64::from(source_height)
                > u64::from(info.coded_width) * u64::from(info.coded_height)
            {
                bail!("ImageIO dimensions exceed admitted HEIF header dimensions");
            }
            let edge = i64::from(max_edge.unwrap_or(source_width.max(source_height)));
            let edge_number = Owned::checked(
                CFNumberCreate(ptr::null(), 4, (&edge as *const i64).cast()),
                "Allocate HEIF thumbnail size failed",
            )?;
            let options = dictionary(
                &[
                    kCGImageSourceCreateThumbnailFromImageAlways,
                    kCGImageSourceCreateThumbnailWithTransform,
                    kCGImageSourceThumbnailMaxPixelSize,
                    kCGImageSourceShouldCache,
                ],
                &[
                    kCFBooleanTrue,
                    kCFBooleanTrue,
                    edge_number.0,
                    kCFBooleanFalse,
                ],
            )?;
            let native = Owned::checked(
                CGImageSourceCreateThumbnailAtIndex(source.0, 0, options.0),
                "ImageIO HEIF decoding failed: unsupported codec or invalid image",
            )?;
            let (width, height) = (CGImageGetWidth(native.0), CGImageGetHeight(native.0));
            validate_size(u32::try_from(width)?, u32::try_from(height)?)?;
            if width as u64 * height as u64
                > u64::from(info.coded_width) * u64::from(info.coded_height)
            {
                bail!("ImageIO HEIF output exceeds admitted dimensions");
            }
            // ColorSync draws high-precision SDR sources into the explicitly
            // eight-bit sRGB destination below. PQ/HLG are rejected at admission.
            if max_edge.is_none() {
                source_width = width as u32;
                source_height = height as u32;
            }
            let mut pixels = vec![0; width * height * 4];
            let srgb = Owned::checked(
                CGColorSpaceCreateWithName(kCGColorSpaceSRGB),
                "Create sRGB display color space failed",
            )?;
            // Big endian RGBA with premultiplied-last alpha. ColorSync performs
            // the source-profile/NCLX -> sRGB conversion during this draw.
            let context = Owned::checked(
                CGBitmapContextCreate(
                    pixels.as_mut_ptr().cast(),
                    width,
                    height,
                    8,
                    width * 4,
                    srgb.0,
                    0x4001,
                ),
                "Allocate HEIF sRGB bitmap context failed",
            )?;
            CGContextDrawImage(
                context.0,
                Rect {
                    origin: Point { x: 0., y: 0. },
                    size: Size {
                        width: width as f64,
                        height: height as f64,
                    },
                },
                native.0,
            );
            drop(context);
            for pixel in pixels.chunks_exact_mut(4) {
                let alpha = u32::from(pixel[3]);
                for channel in &mut pixel[..3] {
                    *channel = if alpha == 0 {
                        0
                    } else {
                        ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8
                    };
                }
            }
            Ok(DecodedHeif {
                image: DynamicImage::ImageRgba8(
                    RgbaImage::from_raw(width as u32, height as u32, pixels)
                        .context("Invalid ImageIO RGBA buffer")?,
                ),
                icc: None,
                converted_to_srgb: true,
                source_width,
                source_height,
            })
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;
    pub fn decode(_bytes: &[u8], _max_edge: Option<u32>, _info: &HeifInfo) -> Result<DecodedHeif> {
        bail!("Native HEIF preview is unavailable on this platform; original remains indexed")
    }
}

#[cfg(test)]
pub(crate) fn test_header(width: u32, height: u32, rotation: u8, depth: u8) -> Vec<u8> {
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(body);
        b
    }
    let mut dimensions = vec![0; 4];
    dimensions.extend(width.to_be_bytes());
    dimensions.extend(height.to_be_bytes());
    let mut properties = boxed(b"ispe", &dimensions);
    properties.extend(boxed(b"pixi", &[0, 0, 0, 0, 3, depth, depth, depth]));
    properties.extend(boxed(b"irot", &[rotation]));
    properties.extend(boxed(b"colr", b"nclx\0\x01\0\x0d\0\x01\x80"));
    let mut iprp = boxed(b"ipco", &properties);
    iprp.extend(boxed(
        b"ipma",
        &[0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 4, 1, 2, 3, 4],
    ));
    let mut meta = vec![0; 4];
    meta.extend(boxed(b"pitm", &[0, 0, 0, 0, 0, 1]));
    meta.extend(boxed(b"iprp", &iprp));
    let mut file = boxed(b"ftyp", b"heic\0\0\0\0mif1heic");
    file.extend(boxed(b"meta", &meta));
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(
        target_os = "linux",
        not(any(feature = "native-codecs", feature = "rust-heif"))
    ))]
    #[test]
    fn disabled_heif_backend_reports_build_capability_after_admission() {
        let bytes = test_header(64, 48, 0, 8);
        let admitted = working_bytes(&bytes, None).unwrap();
        let error = decode(&bytes, None, admitted).err().unwrap();
        assert!(error.to_string().contains("unavailable in this build"));
        assert!(error.to_string().contains("native-codecs"));
        let error = decode(&bytes, None, 0).err().unwrap();
        assert!(error.to_string().contains("working memory"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn tiny_heif_admission_includes_the_minimum_coded_surface() {
        let bytes = test_header(2, 2, 0, 8);
        assert!(working_bytes(&bytes, None).unwrap() >= 256 * 256 * 40 + 13 * 1024 * 1024);
    }
    #[cfg(all(target_os = "linux", feature = "rust-heif"))]
    #[test]
    fn rust_heif_admission_charges_coding_block_padding() {
        let bytes = test_header(451, 461, 0, 8);
        assert_eq!(inspect(&bytes).unwrap().allocation_pixels, 512 * 512);
        assert!(working_bytes(&bytes, None).unwrap() >= 512 * 512 * 40);
    }
    #[test]
    fn headers_apply_rotation_and_admit_before_native_decode() {
        let bytes = test_header(6000, 4000, 1, 8);
        let info = inspect(&bytes).unwrap();
        assert_eq!((info.width, info.height), (4000, 6000));
        assert_eq!((info.coded_width, info.coded_height), (6000, 4000));
        assert!(working_bytes(&bytes, None).unwrap() >= 24_000_000 * 40);
        let error = decode(&bytes, None, 1024).err().unwrap().to_string();
        assert!(error.contains("working memory"), "{error}");
    }
    #[test]
    fn hdr_and_oversized_headers_fail_without_native_pixel_allocation() {
        let hdr = test_header(100, 100, 0, 17);
        assert!(
            working_bytes(&hdr, None)
                .unwrap_err()
                .to_string()
                .contains("HDR")
        );
        for depth in [10, 12, 16] {
            assert!(working_bytes(&test_header(100, 100, 0, depth), None).is_ok());
        }
        assert!(inspect(&test_header(100_000, 100_000, 0, 8)).is_err());
        let mut pq = test_header(100, 100, 0, 8);
        let offset = pq.windows(4).position(|window| window == b"nclx").unwrap();
        pq[offset + 6..offset + 8].copy_from_slice(&16u16.to_be_bytes());
        assert!(
            working_bytes(&pq, None)
                .unwrap_err()
                .to_string()
                .contains("HDR")
        );
    }
    #[test]
    fn malformed_containers_and_metadata_growth_are_bounded() {
        assert!(inspect(&[0; 7]).is_err());
        let mut bytes = test_header(16, 8, 0, 8);
        bytes[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(inspect(&bytes).is_err());
        let mut deep = Vec::new();
        for _ in 0..=MAX_BOXES {
            deep.extend_from_slice(b"\0\0\0\x08free");
        }
        assert!(
            inspect(&deep)
                .unwrap_err()
                .to_string()
                .contains("box count")
        );
    }
    #[test]
    fn metadata_file_reader_seeks_over_large_sparse_media() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frame.heic");
        let mut file = File::create(&path).unwrap();
        // The metadata can follow a large payload; indexing never reads it.
        file.write_all(b"\x10\0\0\0mdat").unwrap();
        file.seek(SeekFrom::Start(256 * 1024 * 1024)).unwrap();
        file.write_all(&test_header(256, 160, 0, 8)).unwrap();
        drop(file);
        let info = inspect_file(&path).unwrap();
        assert_eq!((info.width, info.height), (256, 160));
    }
    #[test]
    fn metadata_file_reader_rejects_invalid_boxes_after_valid_headers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("frame.heic");
        let valid = test_header(256, 160, 0, 8);
        let ftyp_size = u32::from_be_bytes(valid[..4].try_into().unwrap()) as usize;
        let mut duplicate = valid.clone();
        duplicate.extend_from_slice(&valid[..ftyp_size]);
        assert!(inspect(&duplicate).is_err());
        std::fs::write(&path, duplicate).unwrap();
        assert!(inspect_file(&path).is_err());

        let mut truncated = valid.clone();
        truncated.extend_from_slice(b"short");
        assert!(inspect(&truncated).is_err());
        std::fs::write(&path, truncated).unwrap();
        assert!(inspect_file(&path).is_err());

        let mut excessive = valid;
        for _ in 0..MAX_BOXES {
            excessive.extend_from_slice(b"\0\0\0\x08free");
        }
        assert!(inspect(&excessive).is_err());
        std::fs::write(&path, excessive).unwrap();
        assert!(
            inspect_file(&path)
                .unwrap_err()
                .to_string()
                .contains("box count")
        );
    }
    #[test]
    #[ignore = "Set SHADOWROOM_HEIF_FIXTURE to a real SDR HEIF file; no fixture is shipped"]
    fn actual_native_heif_fixture() {
        let path = std::env::var_os("SHADOWROOM_HEIF_FIXTURE").expect("fixture path");
        let bytes = std::fs::read(&path).unwrap();
        let info = inspect(&bytes).unwrap();
        let admitted = working_bytes(&bytes, None).unwrap();
        let decoded = decode(&bytes, None, admitted).unwrap();
        assert_eq!(
            (decoded.source_width, decoded.source_height),
            (info.width, info.height)
        );
        assert_eq!(
            (decoded.image.width(), decoded.image.height()),
            (info.width, info.height)
        );
        // SDR HEIF can have an auxiliary alpha image. Preserve its alpha rather
        // than requiring every valid fixture to be opaque.
        if let Some(reference) = std::env::var_os("SHADOWROOM_HEIF_REFERENCE") {
            let reference = image::open(reference).unwrap();
            assert_eq!(
                (reference.width(), reference.height()),
                (decoded.image.width(), decoded.image.height())
            );
            let expected_alpha = reference.to_rgba8();
            let actual_alpha = decoded.image.to_rgba8();
            let alpha_error = expected_alpha
                .pixels()
                .zip(actual_alpha.pixels())
                .map(|(a, b)| f64::from(a[3].abs_diff(b[3])))
                .sum::<f64>()
                / f64::from(reference.width())
                / f64::from(reference.height());
            assert!(
                alpha_error < 5.,
                "Native HEIF alpha differs from reference: {alpha_error:.2}"
            );
            let expected = reference
                .resize_exact(16, 16, image::imageops::FilterType::Triangle)
                .to_rgb8();
            let actual = decoded
                .image
                .resize_exact(16, 16, image::imageops::FilterType::Triangle)
                .to_rgb8();
            let mean = expected
                .as_raw()
                .iter()
                .zip(actual.as_raw())
                .map(|(a, b)| f64::from(a.abs_diff(*b)))
                .sum::<f64>()
                / (16. * 16. * 3.);
            assert!(
                mean < 30.,
                "Native HEIF orientation/color differs from reference: mean RGB error {mean:.2}"
            );
            println!("native HEIF vs reference: mean coarse RGB error {mean:.2}");
        }
        let fit =
            crate::media::decode_preview(Path::new(&path), crate::media::PreviewKind::Fit).unwrap();
        assert_eq!(
            (fit.source_width, fit.source_height),
            (info.width, info.height)
        );
        assert!(fit.width <= 2560 && fit.height <= 2560);
        if (fit.width, fit.height) == (decoded.image.width(), decoded.image.height()) {
            let native = decoded.image.to_rgba8();
            assert!(
                fit.rgba
                    .chunks_exact(4)
                    .zip(native.pixels())
                    .all(|(a, b)| a[3] == b[3]),
                "Media color conversion changed native HEIF alpha"
            );
        }
        println!(
            "native HEIF {}: {}×{}, {:?}",
            Path::new(&path).display(),
            info.width,
            info.height,
            fit.color_status
        );
    }
}
