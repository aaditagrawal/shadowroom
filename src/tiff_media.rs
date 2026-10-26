//! Bounded, first-image-only Classic TIFF/BigTIFF decoding (pure Rust `tiff`).
//!
//! Integer samples without ICC are assumed to be encoded sRGB. Unprofiled f32
//! RGB/gray samples are explicitly assumed linear sRGB, clipped to SDR, then
//! encoded with the sRGB transfer function. CMYK requires an embedded profile.
//! ICC conversion consumes native u8/u16/f32 samples before display quantization.
use anyhow::{Context, Result, bail};
use image::{DynamicImage, RgbaImage};
use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Pod, Profile, Transform};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
};
use tiff::decoder::{Decoder, DecodingResult, Limits};

const MAX_SOURCE: u64 = 512 * 1024 * 1024;
const MAX_HEADER: usize = 4 * 1024 * 1024;
const MAX_PROFILE: usize = 4 * 1024 * 1024;
const MAX_PIXELS: u64 = 160_000_000;
const MAX_ENTRIES: usize = 4096;
const MAX_CHUNKS: usize = 200_000;

#[derive(Clone, Debug)]
pub struct TiffInfo {
    /// Stored dimensions; swap for display when orientation is 5..=8.
    pub width: u32,
    pub height: u32,
    pub orientation: u16,
    pub bit_depth: u8,
    /// TIFF SampleFormat: 1 = unsigned integer, 3 = IEEE floating point.
    pub sample_format: u16,
    pub samples_per_pixel: u16,
    pub photometric: u16,
    /// Supported lossless compression: none, LZW, Deflate or PackBits.
    pub compression: u16,
    pub has_icc: bool,
    pub big_tiff: bool,
    /// A following IFD exists. Only the first IFD is decoded, never animated.
    pub has_more_images: bool,
    pub planar: bool,
    pub associated_alpha: bool,
    metadata_bytes: usize,
    profile_bytes: usize,
    chunk_pixels: u64,
}

#[derive(Debug)]
pub struct DecodedTiff {
    pub image: DynamicImage,
    /// Original source profile, retained for metadata; pixels are already sRGB.
    pub icc: Option<Vec<u8>>,
    pub converted_to_srgb: bool,
    /// Stored dimensions, before orientation or thumbnailing.
    pub source_width: u32,
    pub source_height: u32,
}

pub fn is_tiff(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        ["tif", "tiff", "btf", "btiff", "tf8"]
            .iter()
            .any(|e| s.eq_ignore_ascii_case(e))
    })
}

#[derive(Clone, Copy)]
struct Endian(bool);
impl Endian {
    fn uint(self, bytes: &[u8]) -> u64 {
        if self.0 {
            bytes.iter().rev().fold(0, |v, &b| (v << 8) | u64::from(b))
        } else {
            bytes.iter().fold(0, |v, &b| (v << 8) | u64::from(b))
        }
    }
}

struct HeaderReader<R> {
    input: R,
    len: u64,
    read_bytes: usize,
}
impl<R: Read + Seek> HeaderReader<R> {
    fn at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.len)
        {
            bail!("TIFF metadata points outside source");
        }
        self.read_bytes = self
            .read_bytes
            .checked_add(len)
            .context("TIFF header overflow")?;
        if self.read_bytes > MAX_HEADER {
            bail!("TIFF header reads exceed 4 MiB safety limit");
        }
        self.input.seek(SeekFrom::Start(offset))?;
        let mut data = vec![0; len];
        self.input
            .read_exact(&mut data)
            .context("Truncated TIFF metadata")?;
        Ok(data)
    }
}

struct Entry {
    ty: u16,
    count: u64,
    len: usize,
    offset: u64,
    inline: Option<Vec<u8>>,
}
impl Entry {
    fn data<R: Read + Seek>(&self, reader: &mut HeaderReader<R>) -> Result<Vec<u8>> {
        match &self.inline {
            Some(v) => Ok(v.clone()),
            None => reader.at(self.offset, self.len),
        }
    }
    fn unsigned<R: Read + Seek>(&self, r: &mut HeaderReader<R>, e: Endian) -> Result<Vec<u64>> {
        let size = match self.ty {
            1 => 1,
            3 => 2,
            4 | 13 => 4,
            16 | 18 => 8,
            _ => bail!("Unsupported TIFF integer tag type"),
        };
        Ok(self
            .data(r)?
            .chunks_exact(size)
            .map(|b| e.uint(b))
            .collect())
    }
}

fn values<R: Read + Seek>(
    tags: &BTreeMap<u16, Entry>,
    r: &mut HeaderReader<R>,
    e: Endian,
    tag: u16,
) -> Result<Option<Vec<u64>>> {
    tags.get(&tag).map(|v| v.unsigned(r, e)).transpose()
}

fn scalar<R: Read + Seek>(
    tags: &BTreeMap<u16, Entry>,
    r: &mut HeaderReader<R>,
    e: Endian,
    tag: u16,
    default: Option<u64>,
) -> Result<u64> {
    match values(tags, r, e, tag)? {
        Some(v) if v.len() == 1 => Ok(v[0]),
        Some(_) => bail!("Invalid TIFF scalar tag {tag}"),
        None => default.with_context(|| format!("Required TIFF tag {tag} missing")),
    }
}

fn valid_dimensions(w: u64, h: u64) -> Result<()> {
    if w == 0 || h == 0 || w > 100_000 || h > 100_000 || w.saturating_mul(h) > MAX_PIXELS {
        bail!("TIFF dimensions exceed 160 megapixel safety limit");
    }
    Ok(())
}

fn parse<R: Read + Seek>(
    input: R,
    len: u64,
    load_icc: bool,
) -> Result<(TiffInfo, Option<Vec<u8>>)> {
    if len > MAX_SOURCE {
        bail!("TIFF source exceeds 512 MiB safety limit");
    }
    let mut r = HeaderReader {
        input,
        len,
        read_bytes: 0,
    };
    let head = r.at(0, 8)?;
    let endian = match &head[..2] {
        b"II" => Endian(true),
        b"MM" => Endian(false),
        _ => bail!("Invalid TIFF byte order"),
    };
    let big = match endian.uint(&head[2..4]) {
        42 => false,
        43 => true,
        _ => bail!("Invalid TIFF magic"),
    };
    let first = if big {
        if endian.uint(&head[4..6]) != 8 || endian.uint(&head[6..8]) != 0 {
            bail!("Unsupported BigTIFF offset size or reserved field");
        }
        endian.uint(&r.at(8, 8)?)
    } else {
        endian.uint(&head[4..8])
    };
    if first < if big { 16 } else { 8 } {
        bail!("Invalid first TIFF IFD offset");
    }
    let (count_size, entry_size, inline_size) = if big { (8, 20, 8) } else { (2, 12, 4) };
    let count = usize::try_from(endian.uint(&r.at(first, count_size)?))?;
    if count == 0 || count > MAX_ENTRIES {
        bail!("TIFF IFD entry count exceeds safety limit");
    }
    let directory = r.at(first + count_size as u64, count * entry_size + inline_size)?;
    let mut metadata_bytes = count * entry_size + count_size + inline_size + 16;
    let mut tags = BTreeMap::new();
    for field in directory[..count * entry_size].chunks_exact(entry_size) {
        let tag = endian.uint(&field[..2]) as u16;
        let ty = endian.uint(&field[2..4]) as u16;
        let size = match ty {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 | 16 | 17 | 18 => 8,
            _ => bail!("Unsupported TIFF tag field type {ty}"),
        };
        let n = endian.uint(&field[4..entry_size - inline_size]);
        let len = usize::try_from(n.checked_mul(size).context("TIFF tag length overflow")?)?;
        // Account even ignored metadata before allowing `Decoder::new` to see it.
        if len > inline_size {
            metadata_bytes = metadata_bytes
                .checked_add(len)
                .context("TIFF metadata overflow")?;
        }
        if metadata_bytes > MAX_HEADER {
            bail!("TIFF metadata exceeds 4 MiB safety limit");
        }
        let value = &field[entry_size - inline_size..];
        let offset = endian.uint(value);
        let inline = if len <= inline_size {
            Some(value[..len].to_vec())
        } else {
            if offset.checked_add(len as u64).is_none_or(|end| end > r.len) {
                bail!("TIFF tag {tag} points outside source");
            }
            None
        };
        if tags
            .insert(
                tag,
                Entry {
                    ty,
                    count: n,
                    len,
                    offset,
                    inline,
                },
            )
            .is_some()
        {
            bail!("Duplicate TIFF tag {tag}");
        }
    }
    let next = endian.uint(&directory[count * entry_size..]);
    if next != 0 && (next < if big { 16 } else { 8 } || next >= len) {
        bail!("Invalid following TIFF IFD offset");
    }
    let width = scalar(&tags, &mut r, endian, 256, None)?;
    let height = scalar(&tags, &mut r, endian, 257, None)?;
    valid_dimensions(width, height)?;
    let compression = scalar(&tags, &mut r, endian, 259, Some(1))?;
    // Embedded JPEG/WebP dimensions can allocate independently of the TIFF
    // canvas in the upstream decoder. Do not enter those codecs without a
    // separate bounded inspection of their internal headers.
    if !matches!(compression, 1 | 5 | 8 | 32946 | 32773) {
        bail!("Unsupported TIFF compression (requires uncompressed, LZW, Deflate or PackBits)");
    }
    let spp = scalar(&tags, &mut r, endian, 277, Some(1))?;
    if !(1..=4).contains(&spp) {
        bail!("Unsupported TIFF sample count {spp}");
    }
    let depths = values(&tags, &mut r, endian, 258)?.unwrap_or_else(|| vec![1]);
    let formats = values(&tags, &mut r, endian, 339)?.unwrap_or_else(|| vec![1]);
    for v in [&depths, &formats] {
        if v.is_empty() || (v.len() != 1 && v.len() != spp as usize) || v.iter().any(|n| n != &v[0])
        {
            bail!("Mixed or invalid TIFF sample depths/formats are unsupported");
        }
    }
    let (depth, format) = (depths[0], formats[0]);
    if !matches!((depth, format), (8 | 16, 1) | (32, 3)) {
        bail!("Unsupported TIFF sample format/depth (requires uint8/16 or float32)");
    }
    let photo = scalar(&tags, &mut r, endian, 262, None)?;
    match (photo, spp) {
        (0 | 1, 1) | (1, 2) | (2, 3 | 4) | (5, 4) => {}
        (3, _) => bail!("Palette TIFF is unsupported by the TIFF pixel decoder"),
        (6, _) => bail!("YCbCr TIFF color interpretation is unsupported"),
        _ => bail!("Unsupported TIFF photometric interpretation/sample count"),
    }
    if photo == 5 {
        if format != 1 || scalar(&tags, &mut r, endian, 332, Some(1))? != 1 {
            bail!("Unsupported TIFF separated ink set (requires integer CMYK)");
        }
        if scalar(&tags, &mut r, endian, 334, Some(4))? != 4 {
            bail!("Unsupported TIFF CMYK ink count");
        }
    }
    let orientation = scalar(&tags, &mut r, endian, 274, Some(1))?;
    if !(1..=8).contains(&orientation) {
        bail!("Invalid TIFF orientation");
    }
    let planar = scalar(&tags, &mut r, endian, 284, Some(1))?;
    if !matches!(planar, 1 | 2) {
        bail!("Invalid TIFF planar configuration");
    }
    let alpha = (photo == 2 && spp == 4) || (photo == 1 && spp == 2);
    let extras = values(&tags, &mut r, endian, 338)?.unwrap_or_default();
    if (!extras.is_empty() && (!alpha || extras.len() != 1 || !matches!(extras[0], 1 | 2)))
        || (photo == 1 && spp == 2 && extras.is_empty())
    {
        bail!("Unsupported TIFF extra sample interpretation");
    }
    let profile_bytes = match tags.get(&34675) {
        Some(v) if matches!(v.ty, 1 | 7) && v.count > 0 && v.len <= MAX_PROFILE => v.len,
        Some(_) => bail!("Invalid TIFF ICC profile or profile exceeds 4 MiB"),
        None => 0,
    };
    if photo == 5 && profile_bytes == 0 {
        bail!("CMYK TIFF requires an embedded ICC profile for accurate rendering");
    }
    // Avoid silently ignoring TIFF's alternative color characterization tags.
    if profile_bytes == 0
        && [301, 318, 319, 529, 532]
            .iter()
            .any(|t| tags.contains_key(t))
    {
        bail!("TIFF color characterization requires an embedded ICC profile");
    }
    let tiled = tags.contains_key(&324) || tags.contains_key(&325);
    let (offset_tag, size_tag, chunk_pixels, expected_chunks) = if tiled {
        if tags.contains_key(&273) || tags.contains_key(&279) {
            bail!("TIFF declares both strips and tiles");
        }
        let tw = scalar(&tags, &mut r, endian, 322, None)?;
        let th = scalar(&tags, &mut r, endian, 323, None)?;
        valid_dimensions(tw, th)?;
        (324, 325, tw * th, width.div_ceil(tw) * height.div_ceil(th))
    } else {
        let rows = scalar(&tags, &mut r, endian, 278, Some(u32::MAX as u64))?;
        if rows == 0 {
            bail!("Invalid TIFF RowsPerStrip");
        }
        (273, 279, width * rows.min(height), height.div_ceil(rows))
    };
    let expected_chunks = expected_chunks * if planar == 2 { spp } else { 1 };
    if expected_chunks > MAX_CHUNKS as u64 {
        bail!("TIFF strip/tile count exceeds safety limit");
    }
    for tag in [offset_tag, size_tag] {
        if tags.get(&tag).is_none_or(|v| v.count != expected_chunks) {
            bail!("Invalid TIFF strip/tile array length");
        }
    }
    let offsets =
        values(&tags, &mut r, endian, offset_tag)?.context("TIFF chunk offsets missing")?;
    let sizes =
        values(&tags, &mut r, endian, size_tag)?.context("TIFF chunk byte counts missing")?;
    for (&offset, &size) in offsets.iter().zip(&sizes) {
        if size == 0 || offset.checked_add(size).is_none_or(|end| end > len) {
            bail!("Invalid TIFF strip/tile range");
        }
    }
    if compression == 1 {
        let chunk_samples = if planar == 2 { 1 } else { spp };
        let full_chunk_bytes = chunk_pixels * chunk_samples * (depth / 8);
        let chunks_per_plane = expected_chunks / if planar == 2 { spp } else { 1 };
        for (index, &size) in sizes.iter().enumerate() {
            let expected = if tiled {
                full_chunk_bytes
            } else {
                let rows = chunk_pixels / width;
                let row = (index as u64 % chunks_per_plane) * rows;
                width * rows.min(height - row) * chunk_samples * (depth / 8)
            };
            if size < expected {
                bail!("Truncated uncompressed TIFF strip/tile byte count");
            }
        }
    }
    let icc = if load_icc {
        tags.get(&34675).map(|v| v.data(&mut r)).transpose()?
    } else {
        None
    };
    Ok((
        TiffInfo {
            width: width as u32,
            height: height as u32,
            orientation: orientation as u16,
            bit_depth: depth as u8,
            sample_format: format as u16,
            samples_per_pixel: spp as u16,
            photometric: photo as u16,
            compression: compression as u16,
            has_icc: profile_bytes != 0,
            big_tiff: big,
            has_more_images: next != 0,
            planar: planar == 2,
            associated_alpha: extras.first() == Some(&1),
            metadata_bytes,
            profile_bytes,
            chunk_pixels,
        },
        icc,
    ))
}

/// Inspect bounded first-IFD metadata without invoking a pixel decoder.
pub fn inspect(bytes: &[u8]) -> Result<TiffInfo> {
    Ok(parse(Cursor::new(bytes), bytes.len() as u64, false)?.0)
}

/// Seek directly to header/IFD/tag arrays. Does not read strips, tiles or ICC data.
pub fn inspect_file(path: &Path) -> Result<TiffInfo> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    Ok(parse(file, len, false)?.0)
}

fn estimate(info: &TiffInfo, source_len: usize, max_edge: Option<u32>) -> Result<usize> {
    if max_edge == Some(0) {
        bail!("TIFF thumbnail edge must be nonzero");
    }
    let pixels = u64::from(info.width) * u64::from(info.height);
    let native = u64::from(info.samples_per_pixel) * u64::from(info.bit_depth / 8);
    // Full native decode, codec chunk scratch, RGBA, orientation copy and the
    // resampler's f32 intermediates/output. Thumbnailing still decodes full TIFF.
    // Also charge compressed codec buffers, tag expansion, and LittleCMS tables.
    let required = pixels
        .checked_mul(native * 2 + 48)
        .and_then(|n| n.checked_add(info.chunk_pixels * (native * 2 + 8)))
        .and_then(|n| n.checked_add(source_len as u64 * 2))
        .and_then(|n| n.checked_add(info.metadata_bytes as u64 * 16))
        .and_then(|n| n.checked_add(info.profile_bytes as u64 * 32 + 8 * 1024 * 1024))
        .context("TIFF working memory overflow")?;
    usize::try_from(required).context("TIFF working memory exceeds address space")
}

pub fn working_bytes(bytes: &[u8], max_edge: Option<u32>) -> Result<usize> {
    estimate(&inspect(bytes)?, bytes.len(), max_edge)
}

trait Sample: Copy + Default + Pod {
    const GRAY: PixelFormat;
    const RGB: PixelFormat;
    const CMYK: PixelFormat;
    const FLOAT: bool;
    fn unit(self) -> f32;
    fn from_unit(value: f32) -> Self;
}
impl Sample for u8 {
    const GRAY: PixelFormat = PixelFormat::GRAY_8;
    const RGB: PixelFormat = PixelFormat::RGB_8;
    const CMYK: PixelFormat = PixelFormat::CMYK_8;
    const FLOAT: bool = false;
    fn unit(self) -> f32 {
        f32::from(self) / 255.0
    }
    fn from_unit(value: f32) -> Self {
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}
impl Sample for u16 {
    const GRAY: PixelFormat = PixelFormat::GRAY_16;
    const RGB: PixelFormat = PixelFormat::RGB_16;
    const CMYK: PixelFormat = PixelFormat::CMYK_16;
    const FLOAT: bool = false;
    fn unit(self) -> f32 {
        f32::from(self) / 65535.0
    }
    fn from_unit(value: f32) -> Self {
        (value.clamp(0.0, 1.0) * 65535.0).round() as u16
    }
}
impl Sample for f32 {
    const GRAY: PixelFormat = PixelFormat::GRAY_FLT;
    const RGB: PixelFormat = PixelFormat::RGB_FLT;
    const CMYK: PixelFormat = PixelFormat::CMYK_FLT;
    const FLOAT: bool = true;
    fn unit(self) -> f32 {
        self
    }
    fn from_unit(value: f32) -> Self {
        value
    }
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}
fn linear_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn color<T: Sample, const N: usize>(
    data: &[T],
    info: &TiffInfo,
    profile: Option<&Profile>,
    format: PixelFormat,
) -> Result<RgbaImage>
where
    [T; N]: Pod,
{
    let pixels = info.width as usize * info.height as usize;
    let spp = usize::from(info.samples_per_pixel);
    if data.len()
        != pixels
            .checked_mul(spp)
            .context("TIFF sample count overflow")?
    {
        bail!("TIFF decoded sample count differs from inspected dimensions");
    }
    let srgb = Profile::new_srgb();
    let transform = profile
        .map(|p| {
            Transform::<[T; N], [f32; 3]>::new(
                p,
                format,
                &srgb,
                PixelFormat::RGB_FLT,
                Intent::RelativeColorimetric,
            )
            .context("Unsupported TIFF source ICC color transform")
        })
        .transpose()?;
    let has_alpha = spp > N;
    let mut output = RgbaImage::new(info.width, info.height);
    let mut input = [[T::default(); N]; 256];
    let mut converted = [[0.0f32; 3]; 256];
    let mut alphas = [1.0f32; 256];
    for base in (0..pixels).step_by(256) {
        let len = (pixels - base).min(256);
        for i in 0..len {
            let sample = |c: usize| {
                data[if info.planar {
                    c * pixels + base + i
                } else {
                    (base + i) * spp + c
                }]
            };
            let alpha = if has_alpha { sample(N).unit() } else { 1.0 };
            if !alpha.is_finite() {
                bail!("TIFF alpha contains a nonfinite sample");
            }
            alphas[i] = alpha.clamp(0.0, 1.0);
            for c in 0..N {
                let value = sample(c);
                if !value.unit().is_finite() {
                    bail!("TIFF contains a nonfinite color sample");
                }
                input[i][c] = if info.associated_alpha {
                    T::from_unit(if alpha > 0.0 {
                        value.unit() / alpha
                    } else {
                        0.0
                    })
                } else {
                    value
                };
            }
        }
        if let Some(t) = &transform {
            t.transform_pixels(&input[..len], &mut converted[..len]);
        } else {
            for i in 0..len {
                converted[i] = std::array::from_fn(|c| {
                    let v = input[i][if N == 1 { 0 } else { c }].unit();
                    if T::FLOAT {
                        linear_srgb(v.clamp(0.0, 1.0))
                    } else {
                        v
                    }
                });
            }
        }
        for i in 0..len {
            if converted[i].iter().any(|v| !v.is_finite()) {
                bail!("TIFF ICC transform produced nonfinite color");
            }
            output.as_mut()[(base + i) * 4..(base + i + 1) * 4].copy_from_slice(&[
                byte(converted[i][0]),
                byte(converted[i][1]),
                byte(converted[i][2]),
                byte(alphas[i]),
            ]);
        }
    }
    Ok(output)
}

fn render<T: Sample>(data: &[T], info: &TiffInfo, profile: Option<&Profile>) -> Result<RgbaImage>
where
    [T; 1]: Pod,
    [T; 3]: Pod,
    [T; 4]: Pod,
{
    match info.photometric {
        0 | 1 => color::<T, 1>(data, info, profile, T::GRAY),
        2 => color::<T, 3>(data, info, profile, T::RGB),
        5 => color::<T, 4>(data, info, profile, T::CMYK),
        _ => bail!("Unsupported TIFF source color"),
    }
}

fn orient(image: RgbaImage, orientation: u16) -> RgbaImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate90, rotate180, rotate270};
    match orientation {
        2 => flip_horizontal(&image),
        3 => rotate180(&image),
        4 => flip_vertical(&image),
        5 => flip_horizontal(&rotate90(&image)),
        6 => rotate90(&image),
        7 => flip_vertical(&rotate90(&image)),
        8 => rotate270(&image),
        _ => image,
    }
}

/// First page only. Admission is checked before constructing the pixel decoder.
/// Pixels returned here are oriented and sRGB, including the unprofiled assumption.
pub fn decode(bytes: &[u8], max_edge: Option<u32>, admitted_bytes: usize) -> Result<DecodedTiff> {
    let (info, icc) = parse(Cursor::new(bytes), bytes.len() as u64, true)?;
    if estimate(&info, bytes.len(), max_edge)? > admitted_bytes {
        bail!("TIFF decode exceeds admitted working memory");
    }
    let profile = icc
        .as_deref()
        .map(Profile::new_icc)
        .transpose()
        .context("Invalid TIFF ICC profile")?;
    if let Some(p) = &profile {
        let expected = match info.photometric {
            0 | 1 => ColorSpaceSignature::GrayData,
            2 => ColorSpaceSignature::RgbData,
            5 => ColorSpaceSignature::CmykData,
            _ => unreachable!(),
        };
        if p.color_space() != expected
            || p.device_class() == lcms2::ProfileClassSignature::LinkClass
        {
            bail!("TIFF source ICC profile does not match pixel color space");
        }
    }
    let native_bytes = info.width as usize
        * info.height as usize
        * usize::from(info.samples_per_pixel)
        * usize::from(info.bit_depth / 8);
    let mut limits = Limits::default();
    limits.decoding_buffer_size = native_bytes;
    limits.ifd_value_size = MAX_HEADER;
    limits.intermediate_buffer_size = native_bytes.max(bytes.len());
    let mut decoder = Decoder::new(Cursor::new(bytes))
        .context("Invalid TIFF decoder metadata")?
        .with_limits(limits);
    if decoder.dimensions()? != (info.width, info.height) {
        bail!("TIFF decoder dimensions differ from inspected header");
    }
    let mut samples = DecodingResult::U8(Vec::new());
    let layout = decoder
        .read_image_to_buffer(&mut samples)
        .context("Unsupported or corrupt TIFF pixel data")?;
    if layout.complete_len != native_bytes || samples.as_buffer(0).byte_len() != native_bytes {
        bail!("TIFF decoder did not return the complete admitted sample layout");
    }
    let image = match &samples {
        DecodingResult::U8(v) if info.sample_format == 1 && info.bit_depth == 8 => {
            render(v, &info, profile.as_ref())?
        }
        DecodingResult::U16(v) if info.sample_format == 1 && info.bit_depth == 16 => {
            render(v, &info, profile.as_ref())?
        }
        DecodingResult::F32(v) if info.sample_format == 3 && info.bit_depth == 32 => {
            render(v, &info, profile.as_ref())?
        }
        _ => bail!("TIFF decoder sample type differs from inspected header"),
    };
    drop(samples);
    drop(decoder);
    let image = DynamicImage::ImageRgba8(orient(image, info.orientation));
    let image = match max_edge {
        Some(edge) if image.width().max(image.height()) > edge => {
            crate::media::resize_triangle_edge(&image, edge)
        }
        _ => image,
    };
    Ok(DecodedTiff {
        image,
        icc,
        converted_to_srgb: true,
        source_width: info.width,
        source_height: info.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use tiff::{
        encoder::{Rational, TiffEncoder, TiffKind, TiffKindBig, TiffKindStandard, colortype},
        tags::{ResolutionUnit, Tag},
    };

    // Every pixel fixture is encoded by the upstream TIFF encoder, including
    // BigTIFF. Header mutations below are confined to intentional corrupt cases.
    fn encode_kind<C: colortype::ColorType, K: TiffKind>(
        data: &[C::Inner],
        orientation: u16,
        icc: Option<&[u8]>,
        alpha: Option<u16>,
    ) -> Vec<u8>
    where
        [C::Inner]: tiff::encoder::TiffValue,
    {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::<_, K>::new_generic(&mut cursor).unwrap();
            let mut image = encoder.new_image::<C>(2, 3).unwrap();
            image
                .encoder()
                .write_tag(Tag::Orientation, orientation)
                .unwrap();
            image.resolution(ResolutionUnit::Inch, Rational { n: 300, d: 1 });
            if let Some(profile) = icc {
                image
                    .encoder()
                    .write_tag(Tag::Unknown(34675), profile)
                    .unwrap();
            }
            if let Some(alpha) = alpha {
                image
                    .encoder()
                    .write_tag(Tag::ExtraSamples, &[alpha][..])
                    .unwrap();
            }
            image.write_data(data).unwrap();
        }
        cursor.into_inner()
    }

    fn fixture<C: colortype::ColorType>(
        data: &[C::Inner],
        big: bool,
        orientation: u16,
        icc: Option<&[u8]>,
        alpha: Option<u16>,
    ) -> Vec<u8>
    where
        [C::Inner]: tiff::encoder::TiffValue,
    {
        if big {
            encode_kind::<C, TiffKindBig>(data, orientation, icc, alpha)
        } else {
            encode_kind::<C, TiffKindStandard>(data, orientation, icc, alpha)
        }
    }

    fn display(bytes: &[u8]) -> DecodedTiff {
        decode(bytes, None, working_bytes(bytes, None).unwrap()).unwrap()
    }

    fn linear_profile() -> Vec<u8> {
        let white = lcms2::CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = lcms2::CIExyYTRIPLE {
            Red: lcms2::CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.0,
            },
            Green: lcms2::CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.0,
            },
            Blue: lcms2::CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.0,
            },
        };
        let gamma = lcms2::ToneCurve::new(1.0);
        Profile::new_rgb(&white, &primaries, &[&gamma, &gamma, &gamma])
            .unwrap()
            .icc()
            .unwrap()
    }

    // A tiny ICC v2 input profile with an actual four-input LUT16 AToB0.
    // LittleCMS independently parses/validates it and supplies reference colors.
    // No system ICC files or platform-specific test dependencies are needed.
    fn cmyk_profile() -> Vec<u8> {
        let mut lut = vec![0u8; 52];
        lut[..4].copy_from_slice(b"mft2");
        lut[8..12].copy_from_slice(&[4, 3, 2, 0]);
        for offset in [12, 28, 44] {
            lut[offset..offset + 4].copy_from_slice(&65536u32.to_be_bytes());
        }
        lut[48..52].copy_from_slice(&[0, 2, 0, 2]);
        for _ in 0..4 {
            lut.extend_from_slice(&[0, 0, 255, 255]);
        }
        for c in 0..2 {
            for m in 0..2 {
                for y in 0..2 {
                    for k in 0..2 {
                        let rgb = [(1 - c) as f64, (1 - m) as f64, (1 - y) as f64];
                        let scale = (1 - k) as f64;
                        // D50-adapted sRGB matrix; ICC XYZ LUT16 range is 0..2.
                        for row in [
                            [0.4361, 0.3851, 0.1431],
                            [0.2225, 0.7169, 0.0606],
                            [0.0139, 0.0971, 0.7142],
                        ] {
                            let xyz = row.iter().zip(rgb).map(|(a, b)| a * b).sum::<f64>() * scale;
                            lut.extend_from_slice(&((xyz * 32768.0).round() as u16).to_be_bytes());
                        }
                    }
                }
            }
        }
        for _ in 0..3 {
            lut.extend_from_slice(&[0, 0, 255, 255]);
        }
        let mut profile = vec![0u8; 176];
        profile[8..12].copy_from_slice(&0x02100000u32.to_be_bytes());
        profile[12..16].copy_from_slice(b"scnr");
        profile[16..20].copy_from_slice(b"CMYK");
        profile[20..24].copy_from_slice(b"XYZ ");
        profile[36..40].copy_from_slice(b"acsp");
        for (offset, value) in [(68, 0.9642f64), (72, 1.0), (76, 0.8249)] {
            profile[offset..offset + 4]
                .copy_from_slice(&((value * 65536.0).round() as u32).to_be_bytes());
        }
        profile[128..132].copy_from_slice(&2u32.to_be_bytes());
        profile[132..136].copy_from_slice(b"wtpt");
        profile[136..140].copy_from_slice(&156u32.to_be_bytes());
        profile[140..144].copy_from_slice(&20u32.to_be_bytes());
        profile[144..148].copy_from_slice(b"A2B0");
        profile[148..152].copy_from_slice(&176u32.to_be_bytes());
        profile[152..156].copy_from_slice(&(lut.len() as u32).to_be_bytes());
        profile[156..160].copy_from_slice(b"XYZ ");
        for (offset, value) in [(164, 0.9642f64), (168, 1.0), (172, 0.8249)] {
            profile[offset..offset + 4]
                .copy_from_slice(&((value * 65536.0).round() as u32).to_be_bytes());
        }
        profile.extend_from_slice(&lut);
        let size = profile.len() as u32;
        profile[..4].copy_from_slice(&size.to_be_bytes());
        Profile::new_icc(&profile).unwrap();
        profile
    }

    fn planar_kind<K: TiffKind>() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::<_, K>::new_generic(&mut cursor).unwrap();
            let mut directory = encoder.image_directory().unwrap();
            let planes = [
                [0u16, 10000, 20000, 30000, 40000, 65535],
                [32768; 6],
                [65535; 6],
            ];
            let offsets: Vec<u64> = planes
                .iter()
                .map(|p| directory.write_data(&p[..]).unwrap())
                .collect();
            directory.write_tag(Tag::ImageWidth, 2u32).unwrap();
            directory.write_tag(Tag::ImageLength, 3u32).unwrap();
            directory
                .write_tag(Tag::BitsPerSample, &[16u16; 3][..])
                .unwrap();
            directory
                .write_tag(Tag::SampleFormat, &[1u16; 3][..])
                .unwrap();
            directory
                .write_tag(Tag::PhotometricInterpretation, 2u16)
                .unwrap();
            directory.write_tag(Tag::Compression, 1u16).unwrap();
            directory.write_tag(Tag::SamplesPerPixel, 3u16).unwrap();
            directory.write_tag(Tag::PlanarConfiguration, 2u16).unwrap();
            directory.write_tag(Tag::RowsPerStrip, 3u32).unwrap();
            directory
                .write_tag(Tag::StripOffsets, &offsets[..])
                .unwrap();
            directory
                .write_tag(Tag::StripByteCounts, &[12u64; 3][..])
                .unwrap();
            directory.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn planar_rgb16_reads_all_planes_for_classic_and_bigtiff() {
        for bytes in [
            planar_kind::<TiffKindStandard>(),
            planar_kind::<TiffKindBig>(),
        ] {
            assert!(inspect(&bytes).unwrap().planar);
            let image = display(&bytes).image.to_rgba8();
            assert_eq!(image.get_pixel(0, 0).0, [0, 128, 255, 255]);
            assert_eq!(image.get_pixel(1, 2).0, [255, 128, 255, 255]);
        }
    }

    #[test]
    fn padded_tiled_rgb_and_white_is_zero_gray_are_decoded_correctly() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::new_big(&mut cursor).unwrap();
            let mut directory = encoder.image_directory().unwrap();
            let data: Vec<u8> = (0..16 * 16).flat_map(|p| [p as u8, 25, 50]).collect();
            let offset = directory.write_data(&data[..]).unwrap();
            directory.write_tag(Tag::ImageWidth, 2u32).unwrap();
            directory.write_tag(Tag::ImageLength, 3u32).unwrap();
            directory
                .write_tag(Tag::BitsPerSample, &[8u16; 3][..])
                .unwrap();
            directory
                .write_tag(Tag::PhotometricInterpretation, 2u16)
                .unwrap();
            directory.write_tag(Tag::SamplesPerPixel, 3u16).unwrap();
            directory.write_tag(Tag::TileWidth, 16u32).unwrap();
            directory.write_tag(Tag::TileLength, 16u32).unwrap();
            directory
                .write_tag(Tag::TileOffsets, &[offset][..])
                .unwrap();
            directory
                .write_tag(Tag::TileByteCounts, &[data.len() as u64][..])
                .unwrap();
            directory.finish().unwrap();
        }
        let image = display(cursor.get_ref()).image.to_rgba8();
        let red: Vec<u8> = image.pixels().map(|p| p[0]).collect();
        assert_eq!(red, [0, 1, 16, 17, 32, 33]);
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::new(&mut cursor).unwrap();
            let mut image = encoder.new_image::<colortype::Gray16>(2, 3).unwrap();
            image
                .encoder()
                .write_tag(Tag::PhotometricInterpretation, 0u16)
                .unwrap();
            image
                .write_data(&[0u16, 65535, 32768, 32768, 65535, 0])
                .unwrap();
        }
        let image = display(cursor.get_ref()).image.to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [255; 4]);
        assert_eq!(image.get_pixel(1, 0).0, [0, 0, 0, 255]);
    }

    #[test]
    fn lossless_codecs_and_horizontal_prediction_keep_native_samples() {
        use tiff::encoder::{Compression, DeflateLevel, Predictor};
        for compression in [
            Compression::Lzw,
            Compression::Deflate(DeflateLevel::default()),
            Compression::Packbits,
        ] {
            let mut cursor = Cursor::new(Vec::new());
            {
                let mut encoder = TiffEncoder::new_big(&mut cursor)
                    .unwrap()
                    .with_compression(compression)
                    .with_predictor(Predictor::Horizontal);
                encoder
                    .write_image::<colortype::RGB16>(2, 3, &[65535, 32768, 0].repeat(6))
                    .unwrap();
            }
            let image = display(cursor.get_ref()).image.to_rgba8();
            assert!(image.pixels().all(|p| p.0 == [255, 128, 0, 255]));
        }
    }

    #[test]
    fn classic_and_bigtiff_all_eight_orientations_and_resolution() {
        let data: Vec<u8> = (0u8..6).flat_map(|p| [p * 40, 5, 7]).collect();
        let expected = [
            [0, 1, 2, 3, 4, 5],
            [1, 0, 3, 2, 5, 4],
            [5, 4, 3, 2, 1, 0],
            [4, 5, 2, 3, 0, 1],
            [0, 2, 4, 1, 3, 5],
            [4, 2, 0, 5, 3, 1],
            [5, 3, 1, 4, 2, 0],
            [1, 3, 5, 0, 2, 4],
        ];
        for big in [false, true] {
            for orientation in 1..=8 {
                let bytes = fixture::<colortype::RGB8>(&data, big, orientation, None, None);
                let info = inspect(&bytes).unwrap();
                assert_eq!(
                    (info.width, info.height, info.big_tiff, info.orientation),
                    (2, 3, big, orientation)
                );
                let decoded = display(&bytes);
                assert_eq!((decoded.source_width, decoded.source_height), (2, 3));
                let image = decoded.image.to_rgba8();
                assert_eq!(
                    image.dimensions(),
                    if orientation >= 5 { (3, 2) } else { (2, 3) }
                );
                let indices: Vec<u8> = image.pixels().map(|p| p[0] / 40).collect();
                assert_eq!(indices, expected[orientation as usize - 1]);
                assert!(image.pixels().all(|p| p[3] == 255));
            }
        }
    }

    #[test]
    fn rgba_8_16_and_float_preserve_unassociated_and_associated_alpha() {
        for big in [false, true] {
            let bytes =
                fixture::<colortype::RGBA8>(&[100, 50, 25, 128].repeat(6), big, 1, None, Some(2));
            assert_eq!(
                display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
                [100, 50, 25, 128]
            );
            let bytes = fixture::<colortype::RGBA16>(
                &[32768, 16384, 8192, 32768].repeat(6),
                big,
                6,
                None,
                Some(1),
            );
            assert_eq!(
                display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
                [255, 128, 64, 128]
            );
            let bytes = fixture::<colortype::RGBA32Float>(
                &[0.25, 0.125, 0.0, 0.5].repeat(6),
                big,
                5,
                None,
                Some(1),
            );
            assert_eq!(
                display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
                [188, 137, 0, 128]
            );
        }
    }

    #[test]
    fn rgb16_icc_before_quantization_and_float_icc_native() {
        let profile = linear_profile();
        for big in [false, true] {
            let bytes = fixture::<colortype::RGB16>(
                &[128, 32768, 65535].repeat(6),
                big,
                1,
                Some(&profile),
                None,
            );
            let decoded = display(&bytes);
            let p = decoded.image.to_rgba8().get_pixel(0, 0).0;
            // 128/65535 -> sRGB 6; quantizing before conversion would erase it.
            assert!((i16::from(p[0]) - 6).abs() <= 1);
            assert!((i16::from(p[1]) - 188).abs() <= 1);
            assert_eq!(p[2..], [255, 255]);
            assert_eq!(decoded.icc.as_deref(), Some(profile.as_slice()));
            assert!(decoded.converted_to_srgb);
            let floats = fixture::<colortype::RGB32Float>(
                &[0.003, 0.5, 1.0].repeat(6),
                big,
                1,
                Some(&profile),
                None,
            );
            let p = display(&floats).image.to_rgba8().get_pixel(0, 0).0;
            assert!((i16::from(p[0]) - 10).abs() <= 1);
            assert!((i16::from(p[1]) - 188).abs() <= 1);
        }
    }

    #[test]
    fn unprofiled_float_linear_srgb_and_gray_native_depths() {
        let bytes =
            fixture::<colortype::RGB32Float>(&[0.5, -1.0, 2.0].repeat(6), true, 1, None, None);
        assert_eq!(
            display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
            [188, 0, 255, 255]
        );
        let bytes = fixture::<colortype::Gray8>(&[0, 51, 102, 153, 204, 255], false, 1, None, None);
        assert_eq!(display(&bytes).image.to_rgba8().get_pixel(1, 2).0, [255; 4]);
        let bytes = fixture::<colortype::Gray16>(&[32768; 6], true, 1, None, None);
        assert_eq!(
            display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
            [128, 128, 128, 255]
        );
        let bytes = fixture::<colortype::Gray32Float>(&[0.5; 6], true, 1, None, None);
        assert_eq!(
            display(&bytes).image.to_rgba8().get_pixel(0, 0).0,
            [188, 188, 188, 255]
        );
        let gamma = lcms2::ToneCurve::new(1.0);
        let profile = Profile::new_gray(&lcms2::CIExyY::d50(), &gamma)
            .unwrap()
            .icc()
            .unwrap();
        let bytes = fixture::<colortype::Gray16>(&[32768; 6], false, 1, Some(&profile), None);
        let p = display(&bytes).image.to_rgba8().get_pixel(0, 0).0;
        assert!(p[..3].iter().all(|v| (i16::from(*v) - 188).abs() <= 1));
    }

    #[test]
    fn cmyk_8_and_16_use_actual_four_channel_icc_transform() {
        let profile_bytes = cmyk_profile();
        let profile = Profile::new_icc(&profile_bytes).unwrap();
        let srgb = Profile::new_srgb();
        let mut expected = [[0.0f32; 3]];
        let transform = Transform::<[u16; 4], [f32; 3]>::new(
            &profile,
            PixelFormat::CMYK_16,
            &srgb,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
        )
        .unwrap();
        transform.transform_pixels(&[[5000, 30000, 45000, 12000]], &mut expected);
        let rgb = expected[0].map(byte);
        for big in [false, true] {
            let bytes = fixture::<colortype::CMYK16>(
                &[5000, 30000, 45000, 12000].repeat(6),
                big,
                1,
                Some(&profile_bytes),
                None,
            );
            assert_eq!(
                &display(&bytes).image.to_rgba8().get_pixel(0, 0).0[..3],
                &rgb
            );
            let bytes = fixture::<colortype::CMYK8>(
                &[0, 255, 255, 0].repeat(6),
                big,
                1,
                Some(&profile_bytes),
                None,
            );
            let p = display(&bytes).image.to_rgba8().get_pixel(0, 0).0;
            assert!(p[0] > 240 && p[1] < 20 && p[2] < 20, "{p:?}");
        }
        let bytes = fixture::<colortype::CMYK8>(&[0; 24], false, 1, None, None);
        assert!(
            inspect(&bytes)
                .unwrap_err()
                .to_string()
                .contains("requires an embedded ICC")
        );
    }

    #[test]
    fn first_page_only_is_explicit_and_thumbnail_dimensions_are_bounded() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::new_big(&mut cursor).unwrap();
            encoder
                .write_image::<colortype::RGB8>(2, 3, &[255, 0, 0].repeat(6))
                .unwrap();
            encoder
                .write_image::<colortype::RGB8>(3, 2, &[0, 0, 255].repeat(6))
                .unwrap();
        }
        let bytes = cursor.into_inner();
        assert!(inspect(&bytes).unwrap().has_more_images);
        let decoded = decode(&bytes, Some(2), working_bytes(&bytes, Some(2)).unwrap()).unwrap();
        assert_eq!(decoded.image.dimensions(), (1, 2));
        assert!(
            decoded
                .image
                .to_rgba8()
                .pixels()
                .all(|p| p.0 == [255, 0, 0, 255])
        );
    }

    #[test]
    fn bounded_seek_inspection_and_file_api_agree() {
        struct Counted {
            cursor: Cursor<Vec<u8>>,
            read: std::rc::Rc<std::cell::Cell<usize>>,
        }
        impl Read for Counted {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = self.cursor.read(out)?;
                self.read.set(self.read.get() + n);
                Ok(n)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.cursor.seek(pos)
            }
        }
        let profile = linear_profile();
        let mut bytes = fixture::<colortype::RGB8>(&[0; 18], true, 8, Some(&profile), None);
        let original_len = bytes.len();
        bytes.resize(12 * 1024 * 1024, 0); // Metadata reads do not scale with file size.
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = Counted {
            cursor: Cursor::new(bytes.clone()),
            read: count.clone(),
        };
        let info = parse(reader, bytes.len() as u64, false).unwrap().0;
        assert!(info.has_icc);
        assert!(count.get() < 1024, "read {} bytes", count.get());
        let path = std::env::temp_dir().join(format!(
            "shadowroom-tiff-{}-{}.btf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, &bytes[..original_len]).unwrap();
        let file_info = inspect_file(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            (file_info.width, file_info.height, file_info.orientation),
            (2, 3, 8)
        );
    }

    #[test]
    fn admission_limits_corruption_and_unsupported_colors_are_explicit() {
        let bytes = fixture::<colortype::RGB8>(&[0; 18], false, 1, None, None);
        let required = working_bytes(&bytes, None).unwrap();
        assert!(
            decode(&bytes, None, required - 1)
                .unwrap_err()
                .to_string()
                .contains("admitted")
        );
        assert!(working_bytes(&bytes, Some(0)).is_err());
        for len in 0..bytes.len() {
            assert!(inspect(&bytes[..len]).is_err(), "accepted truncation {len}");
        }
        let bad = fixture::<colortype::RGB8>(&[0; 18], true, 9, None, None);
        assert!(inspect(&bad).is_err());
        let bad = fixture::<colortype::RGB32Float>(&[f32::NAN; 18], false, 1, None, None);
        assert!(decode(&bad, None, working_bytes(&bad, None).unwrap()).is_err());
        let bad = fixture::<colortype::RGB8>(&[0; 18], false, 1, Some(b"invalid ICC"), None);
        assert!(decode(&bad, None, working_bytes(&bad, None).unwrap()).is_err());
        let wrong = Profile::new_srgb().icc().unwrap();
        let bad = fixture::<colortype::CMYK16>(&[0; 24], true, 1, Some(&wrong), None);
        assert!(decode(&bad, None, working_bytes(&bad, None).unwrap()).is_err());
        let bad = fixture::<colortype::YCbCr8>(&[0; 18], false, 1, None, None);
        assert!(inspect(&bad).unwrap_err().to_string().contains("YCbCr"));
        let bad = fixture::<colortype::RGB32>(&[0; 18], false, 1, None, None);
        assert!(inspect(&bad).is_err());
        // The encoder writes an uncompressed strip, while the explicit tag
        // requests JPEG: reject by metadata before entering an embedded codec.
        let mut jpeg = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::new(&mut jpeg).unwrap();
            let mut image = encoder.new_image::<colortype::RGB8>(2, 3).unwrap();
            image.encoder().write_tag(Tag::Compression, 7u16).unwrap();
            image.write_data(&[0; 18]).unwrap();
        }
        assert!(
            inspect(jpeg.get_ref())
                .unwrap_err()
                .to_string()
                .contains("compression")
        );
        let mut oversized = Cursor::new(Vec::new());
        {
            let mut encoder = TiffEncoder::new(&mut oversized).unwrap();
            let _image = encoder
                .new_image::<colortype::RGB8>(100_000, 100_000)
                .unwrap();
        }
        assert!(
            inspect(oversized.get_ref())
                .unwrap_err()
                .to_string()
                .contains("160 megapixel")
        );
        assert!(parse(Cursor::new(&bytes), MAX_SOURCE + 1, false).is_err());
        // The actual encoder creates the ICC tag and out-of-line storage.
        let bad =
            fixture::<colortype::RGB8>(&[0; 18], true, 1, Some(&vec![0; MAX_PROFILE + 1]), None);
        assert!(inspect(&bad).is_err());
    }

    #[test]
    fn extensions_are_case_insensitive_and_specific() {
        for path in ["a.tif", "a.TIFF", "a.btf", "a.BTIFF", "a.tf8"] {
            assert!(is_tiff(Path::new(path)));
        }
        for path in ["a.jpg", "a.tiff.exe", "tiff"] {
            assert!(!is_tiff(Path::new(path)));
        }
    }

    /// Optional native-harness artifacts, all generated by the upstream encoder.
    #[test]
    #[ignore = "set SHADOWROOM_TIFF_FIXTURES to an output directory to export fixtures"]
    fn export_native_harness_fixtures() {
        let directory =
            std::env::var_os("SHADOWROOM_TIFF_FIXTURES").expect("fixture output directory");
        let directory = Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        let linear = linear_profile();
        let cmyk = cmyk_profile();
        for (name, bytes) in [
            (
                "rgb8-classic.tif",
                fixture::<colortype::RGB8>(&[255, 0, 128].repeat(6), false, 1, None, None),
            ),
            (
                "rgba16-oriented-bigtiff.tiff",
                fixture::<colortype::RGBA16>(
                    &[16384, 32768, 65535, 32768].repeat(6),
                    true,
                    6,
                    Some(&linear),
                    Some(2),
                ),
            ),
            (
                "rgb32f-linear-bigtiff.btf",
                fixture::<colortype::RGB32Float>(&[0.5, 0.0, 1.0].repeat(6), true, 8, None, None),
            ),
            (
                "gray16-bigtiff.tif",
                fixture::<colortype::Gray16>(&[32768; 6], true, 1, None, None),
            ),
            (
                "cmyk16-icc.tiff",
                fixture::<colortype::CMYK16>(
                    &[5000, 30000, 45000, 12000].repeat(6),
                    false,
                    1,
                    Some(&cmyk),
                    None,
                ),
            ),
            ("planar-rgb16-bigtiff.tif", planar_kind::<TiffKindBig>()),
        ] {
            // Export only fixtures independently inspected and decoded here.
            let _ = display(&bytes);
            std::fs::write(directory.join(name), bytes).unwrap();
        }
    }
}
