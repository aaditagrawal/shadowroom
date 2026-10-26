//! Portable JPEG XL inspection and first-displayed-frame SDR decoding.
//!
//! Admission is conservative, not a process-wide allocator: upstream's tracker
//! bounds pixel grids; entropy tables, frame bookkeeping and LCMS are untracked.
//! Container auxiliary boxes never reach upstream (notably unbounded `brob`).
use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, RgbaImage};
use jxl_bitstream::Bitstream;
use jxl_oxide::{AllocTracker, EnumColourEncoding, InitializeResult, JxlImage, RenderingIntent};
use jxl_oxide_common::Bundle;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const MAX_SOURCE: usize = 512 * 1024 * 1024;
const MAX_PROFILE: usize = 4 * 1024 * 1024;
// Upstream ICC command expansion can exceed its declared output size before
// validation. A command emits at most 36 bytes per encoded command byte, plus
// the 132-byte header. Bound that worst case before upstream allocates anything.
const MAX_ENCODED_PROFILE: usize = (MAX_PROFILE - 132) / 36;
const MAX_HEADER: usize = 64 * 1024;
const MAX_PIXELS: u64 = 160_000_000;
const MAX_BOXES: usize = 4096;
const FIXED_ALLOWANCE: usize = 32 * 1024 * 1024;
const MIN_GRID_BUDGET: usize = 16 * 1024 * 1024;
const CHUNK: usize = 4096;

#[derive(Clone, Debug)]
pub struct JxlInfo {
    /// Display dimensions, including the codestream's orientation.
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub orientation: u8,
    pub bit_depth: u8,
    pub floating_point: bool,
    pub has_icc: bool,
    pub has_alpha: bool,
    pub associated_alpha: bool,
    pub animated: bool,
    /// Animated files are represented by their first displayed frame only.
    pub first_frame_only: bool,
    /// Header-declared HDR; ICC HDR may additionally be detected at decode time.
    pub hdr: bool,
    pub extra_channels: usize,
}

pub struct DecodedJxl {
    pub image: DynamicImage,
    /// Original source profile, for provenance only when converted_to_srgb=true.
    pub icc: Option<Vec<u8>>,
    pub converted_to_srgb: bool,
    pub source_width: u32,
    pub source_height: u32,
}

pub fn is_jxl(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("jxl"))
}

/// Borrow codestream pieces without copying source payloads or decompressing
/// metadata. Validate all box sizes and multipart sequence numbers up front.
fn codestream(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    ensure!(
        bytes.len() <= MAX_SOURCE,
        "JPEG XL source exceeds 512 MiB safety limit"
    );
    if bytes.starts_with(&[0xff, 0x0a]) {
        return Ok(vec![bytes]);
    }
    ensure!(
        bytes.starts_with(b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a"),
        "Invalid JPEG XL signature"
    );
    let mut rest = &bytes[12..];
    let mut pieces = Vec::new();
    let (mut whole, mut partial, mut last) = (false, false, false);
    let mut expected_index = 0u32;
    let mut boxes = 0;
    while !rest.is_empty() {
        boxes += 1;
        ensure!(boxes <= MAX_BOXES, "JPEG XL box count exceeds safety limit");
        let header = rest.get(..8).context("Truncated JPEG XL box header")?;
        let kind = &header[4..8];
        let short = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, header_len) = match short {
            0 => (rest.len(), 8),
            1 => {
                let n = u64::from_be_bytes(
                    rest.get(8..16)
                        .context("Truncated extended JPEG XL box")?
                        .try_into()
                        .unwrap(),
                );
                (usize::try_from(n).context("JPEG XL box size overflow")?, 16)
            }
            n => (n as usize, 8),
        };
        ensure!(
            size >= header_len && size <= rest.len(),
            "Invalid JPEG XL box size"
        );
        let payload = &rest[header_len..size];
        match kind {
            b"jxlc" => {
                ensure!(
                    !whole && !partial,
                    "Duplicate or mixed JPEG XL codestream boxes"
                );
                whole = true;
                pieces.push(payload);
            }
            b"jxlp" => {
                ensure!(
                    !whole && !last,
                    "Invalid JPEG XL multipart codestream order"
                );
                let index = u32::from_be_bytes(
                    payload
                        .get(..4)
                        .context("Truncated JPEG XL part index")?
                        .try_into()
                        .unwrap(),
                );
                ensure!(
                    index & 0x7fff_ffff == expected_index,
                    "Out-of-order JPEG XL codestream part"
                );
                expected_index += 1;
                partial = true;
                last = index & 0x8000_0000 != 0;
                pieces.push(&payload[4..]);
            }
            // Exif/XMP/JUMBF/JPEG reconstruction and brob are deliberately not
            // needed for image display. In particular do not inflate brob.
            _ => {}
        }
        rest = &rest[size..];
    }
    ensure!(
        whole || (partial && last),
        "Missing or incomplete JPEG XL codestream"
    );
    Ok(pieces)
}

fn validate_size(width: u32, height: u32) -> Result<()> {
    ensure!(
        width > 0
            && height > 0
            && width <= 100_000
            && height <= 100_000
            && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "JPEG XL dimensions exceed the 160 megapixel safety limit"
    );
    Ok(())
}

fn prefix(pieces: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAX_HEADER);
    for part in pieces {
        out.extend_from_slice(&part[..part.len().min(MAX_HEADER - out.len())]);
        if out.len() == MAX_HEADER {
            break;
        }
    }
    out
}

fn info_from_parts(pieces: &[&[u8]]) -> Result<JxlInfo> {
    let header_bytes = prefix(pieces);
    info_from_prefix(&header_bytes)
}

fn info_from_prefix(header_bytes: &[u8]) -> Result<JxlInfo> {
    let mut bits = Bitstream::new(header_bytes);
    ensure!(
        bits.read_bits(16)? == 0x0aff,
        "Invalid JPEG XL codestream signature"
    );
    // Validate dimensions before parsing the remaining metadata. SizeHeader
    // allocates no pixel or variable-size buffers.
    let size = jxl_oxide::image::SizeHeader::parse(&mut bits, ())?;
    validate_size(size.width, size.height)?;
    let mut bits = Bitstream::new(header_bytes);
    let header = jxl_oxide::ImageHeader::parse(&mut bits, ())
        .context("Invalid or oversized JPEG XL image header (64 KiB limit)")?;
    let meta = &header.metadata;
    ensure!(
        meta.ec_info.len() <= 8,
        "JPEG XL images with more than eight extra channels are unsupported"
    );
    if let Some(preview) = &meta.preview {
        validate_size(preview.width, preview.height)?;
    }
    if meta.colour_encoding.want_icc() {
        check_icc_size(&mut bits)?;
    }
    let floating_point = matches!(
        meta.bit_depth,
        jxl_oxide::image::BitDepth::FloatSample { .. }
    );
    let hdr_transfer = matches!(&meta.colour_encoding,
        jxl_oxide::color::ColourEncoding::Enum(e) if matches!(e.tf, jxl_oxide::color::TransferFunction::Pq | jxl_oxide::color::TransferFunction::Hlg));
    Ok(JxlInfo {
        width: header.width_with_orientation(),
        height: header.height_with_orientation(),
        coded_width: size.width,
        coded_height: size.height,
        orientation: meta.orientation as u8,
        bit_depth: meta.bit_depth.bits_per_sample() as u8,
        floating_point,
        has_icc: meta.colour_encoding.want_icc(),
        has_alpha: meta.ec_info.iter().any(|e| e.is_alpha()),
        associated_alpha: meta
            .ec_info
            .iter()
            .any(|e| e.alpha_associated() == Some(true)),
        animated: meta.animation.is_some(),
        first_frame_only: meta.animation.is_some(),
        hdr: hdr_transfer || floating_point || meta.tone_mapping.intensity_target > 255.0,
        extra_channels: meta.ec_info.len(),
    })
}

/// Bounded header inspection only; does not claim that compressed pixels are
/// valid. Both the raw codestream and the standard multipart container work.
pub fn inspect(bytes: &[u8]) -> Result<JxlInfo> {
    info_from_parts(&codestream(bytes)?)
}

/// Indexing path: read at most 64 KiB of codestream plus bounded box headers.
/// Seek over auxiliary payloads, including compressed metadata, instead of
/// reading them. No pixel decoder runs, and the file remains read-only.
pub fn inspect_file(path: &Path) -> Result<JxlInfo> {
    let mut file = File::open(path).context("Open JPEG XL header")?;
    let file_len = file.metadata()?.len();
    ensure!(
        file_len <= MAX_SOURCE as u64,
        "JPEG XL source exceeds 512 MiB safety limit"
    );
    let mut signature = [0u8; 12];
    file.read_exact(&mut signature[..2])
        .context("Truncated JPEG XL signature")?;
    file.seek(SeekFrom::Start(0))?;
    if signature[..2] == [0xff, 0x0a] {
        let mut header = Vec::with_capacity(MAX_HEADER);
        file.take(MAX_HEADER as u64).read_to_end(&mut header)?;
        return info_from_prefix(&header);
    }
    file.read_exact(&mut signature)
        .context("Truncated JPEG XL container signature")?;
    ensure!(
        &signature == b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a",
        "Invalid JPEG XL signature"
    );
    let mut position = 12u64;
    let mut header = Vec::with_capacity(MAX_HEADER);
    let (mut whole, mut partial, mut last) = (false, false, false);
    let mut expected_index = 0u32;
    let mut boxes = 0;
    while position < file_len {
        boxes += 1;
        ensure!(boxes <= MAX_BOXES, "JPEG XL box count exceeds safety limit");
        ensure!(file_len - position >= 8, "Truncated JPEG XL box header");
        file.seek(SeekFrom::Start(position))?;
        let mut box_header = [0u8; 8];
        file.read_exact(&mut box_header)?;
        let short = u32::from_be_bytes(box_header[..4].try_into().unwrap());
        let (size, header_len) = match short {
            0 => (file_len - position, 8u64),
            1 => {
                let mut extended = [0u8; 8];
                file.read_exact(&mut extended)
                    .context("Truncated extended JPEG XL box")?;
                (u64::from_be_bytes(extended), 16)
            }
            n => (u64::from(n), 8),
        };
        ensure!(
            size >= header_len && size <= file_len - position,
            "Invalid JPEG XL box size"
        );
        let mut payload_len = size - header_len;
        let is_codestream = match &box_header[4..] {
            b"jxlc" => {
                ensure!(
                    !whole && !partial,
                    "Duplicate or mixed JPEG XL codestream boxes"
                );
                whole = true;
                true
            }
            b"jxlp" => {
                ensure!(
                    !whole && !last && payload_len >= 4,
                    "Invalid JPEG XL multipart codestream order"
                );
                let mut index = [0u8; 4];
                file.read_exact(&mut index)?;
                let index = u32::from_be_bytes(index);
                ensure!(
                    index & 0x7fff_ffff == expected_index,
                    "Out-of-order JPEG XL codestream part"
                );
                expected_index += 1;
                partial = true;
                last = index & 0x8000_0000 != 0;
                payload_len -= 4;
                true
            }
            _ => false,
        };
        if is_codestream && header.len() < MAX_HEADER {
            let count = payload_len.min((MAX_HEADER - header.len()) as u64) as usize;
            let old_len = header.len();
            header.resize(old_len + count, 0);
            file.read_exact(&mut header[old_len..])?;
        }
        position += size;
    }
    ensure!(
        whole || (partial && last),
        "Missing or incomplete JPEG XL codestream"
    );
    info_from_prefix(&header)
}

// The initial eighteen ICC symbols always use entropy context zero. Reading
// just those symbols validates both length varints without inflating a profile.
// Format/API source: jxl-color 0.11.0 src/icc/decode.rs (MIT OR Apache-2.0).
fn check_icc_size(bits: &mut Bitstream<'_>) -> Result<()> {
    let encoded_size = bits.read_u64()?;
    ensure!(
        encoded_size <= MAX_ENCODED_PROFILE as u64,
        "JPEG XL encoded ICC profile exceeds conservative safety limit"
    );
    let mut decoder = jxl_coding::Decoder::parse(bits, 41)
        .map_err(|e| anyhow::anyhow!("Invalid JPEG XL ICC entropy header: {e}"))?;
    decoder
        .begin(bits)
        .map_err(|e| anyhow::anyhow!("Invalid JPEG XL ICC stream: {e}"))?;
    let mut initial = [0u8; 18];
    let count = initial.len().min(encoded_size as usize);
    for symbol in &mut initial[..count] {
        let v = decoder
            .read_varint(bits, 0)
            .map_err(|e| anyhow::anyhow!("Invalid JPEG XL ICC length: {e}"))?;
        ensure!(v < 256, "Invalid JPEG XL ICC symbol");
        *symbol = v as u8;
    }
    let mut rest = &initial[..count];
    let output = icc_varint(&mut rest)?;
    let commands = icc_varint(&mut rest)?;
    ensure!(
        output <= MAX_PROFILE as u64,
        "JPEG XL ICC profile exceeds 4 MiB safety limit"
    );
    ensure!(
        commands
            .checked_add((count - rest.len()) as u64)
            .is_some_and(|n| n <= encoded_size),
        "Invalid JPEG XL ICC command length"
    );
    Ok(())
}

fn icc_varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..63).step_by(7) {
        let (&b, rest) = bytes
            .split_first()
            .context("Truncated JPEG XL ICC length")?;
        *bytes = rest;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("Overflowing JPEG XL ICC length")
}

fn estimate(info: &JxlInfo, source_len: usize, max_edge: u32) -> Result<usize> {
    ensure!(max_edge > 0, "JPEG XL target edge must be positive");
    let pixels = (info.width as usize)
        .checked_mul(info.height as usize)
        .context("JPEG XL pixel count overflow")?;
    // Full-resolution codec grids are necessary even for a thumbnail. Include
    // extra channels, RGBA staging plus resize scratch/output, source buffering,
    // and a fixed allowance for header/ICC/entropy/LCMS allocations.
    let grids = pixels
        .checked_mul(64 + info.extra_channels * 16)
        .context("JPEG XL grid reservation overflow")?
        .max(MIN_GRID_BUDGET);
    pixels
        .checked_mul(16)
        .and_then(|n| n.checked_add(grids))
        .and_then(|n| n.checked_add(source_len.checked_mul(2)?))
        .and_then(|n| n.checked_add(FIXED_ALLOWANCE))
        .context("JPEG XL working memory estimate overflow")
}

/// Returns the reservation the caller must hold throughout decode and resize.
pub fn working_bytes(bytes: &[u8], max_edge: u32) -> Result<usize> {
    estimate(&inspect(bytes)?, bytes.len(), max_edge)
}

pub fn decode(bytes: &[u8], max_edge: u32, admitted: usize) -> Result<DecodedJxl> {
    let pieces = codestream(bytes)?;
    let info = info_from_parts(&pieces)?;
    let required = estimate(&info, bytes.len(), max_edge)?;
    ensure!(
        admitted >= required,
        "JPEG XL decode exceeds admitted working memory budget"
    );
    ensure!(
        !info.hdr,
        "JPEG XL HDR/floating-point rendering requires SDR tone mapping and is not yet supported"
    );
    // jxl-oxide currently transforms color planes without unassociating alpha.
    // Avoid producing incorrect nonlinear colors for associated-alpha sources.
    ensure!(
        !info.associated_alpha,
        "JPEG XL associated-alpha rendering is not yet supported"
    );
    let pixels = info.width as usize * info.height as usize;
    let untracked = pixels
        .checked_mul(16)
        .and_then(|n| n.checked_add(bytes.len() * 2))
        .and_then(|n| n.checked_add(FIXED_ALLOWANCE))
        .context("JPEG XL reserve overflow")?;
    let tracker = AllocTracker::with_limit(
        required
            .checked_sub(untracked)
            .context("JPEG XL tracker reserve underflow")?,
    );
    let mut uninit = JxlImage::builder().alloc_tracker(tracker).build_uninit();
    let mut image = None;
    let mut init_bytes = 0usize;
    'load: for piece in &pieces {
        for chunk in piece.chunks(CHUNK) {
            if let Some(decoder) = image.as_mut() {
                let decoder: &mut JxlImage = decoder;
                decoder
                    .feed_bytes(chunk)
                    .map_err(|e| anyhow::anyhow!("Invalid JPEG XL frame: {e}"))?;
                ensure!(
                    decoder.num_loaded_frames() <= 256,
                    "JPEG XL first frame has too many dependencies"
                );
                if decoder.num_loaded_keyframes() > 0 {
                    break 'load;
                }
            } else {
                init_bytes += chunk.len();
                ensure!(
                    init_bytes <= 8 * 1024 * 1024,
                    "JPEG XL initialization exceeds 8 MiB safety limit"
                );
                uninit
                    .feed_bytes(chunk)
                    .map_err(|e| anyhow::anyhow!("Invalid JPEG XL header: {e}"))?;
                match uninit
                    .try_init()
                    .map_err(|e| anyhow::anyhow!("Invalid JPEG XL initialization: {e}"))?
                {
                    InitializeResult::NeedMoreData(next) => uninit = next,
                    InitializeResult::Initialized(mut next) => {
                        ensure!(
                            next.original_icc()
                                .is_none_or(|icc| icc.len() <= MAX_PROFILE),
                            "JPEG XL ICC profile exceeds safety limit"
                        );
                        ensure!(
                            next.hdr_type().is_none(),
                            "JPEG XL HDR rendering requires SDR tone mapping and is not yet supported"
                        );
                        next.request_color_encoding(EnumColourEncoding::srgb(
                            RenderingIntent::Relative,
                        ));
                        let ready = next.num_loaded_keyframes() > 0;
                        image = Some(next);
                        // Move a fresh unused uninitialized object into this
                        // slot so Rust can prove every next iteration is valid.
                        uninit = JxlImage::builder().build_uninit();
                        if ready {
                            break 'load;
                        }
                    }
                }
            }
        }
    }
    let mut decoder = image.context("Truncated JPEG XL image header")?;
    decoder
        .finalize()
        .map_err(|e| anyhow::anyhow!("Invalid JPEG XL stream: {e}"))?;
    ensure!(
        decoder.num_loaded_keyframes() > 0,
        "Truncated JPEG XL first displayed frame"
    );
    ensure!(
        decoder.num_loaded_frames() <= 256,
        "JPEG XL first frame has too many dependencies"
    );
    let icc = decoder.original_icc().map(<[u8]>::to_vec);
    let rendered = decoder
        .render_frame(0)
        .map_err(|e| anyhow::anyhow!("JPEG XL render failed: {e}"))?;
    let mut stream = rendered.stream();
    ensure!(
        stream.width() == info.width && stream.height() == info.height,
        "JPEG XL rendered dimensions differ from admitted canvas"
    );
    let channels = stream.channels() as usize;
    ensure!(
        channels == 3 || channels == 4,
        "JPEG XL sRGB conversion returned an unsupported pixel format"
    );
    let mut rgba = vec![0u8; pixels * 4];
    // A single float row preserves precision until sRGB conversion and avoids
    // the upstream untracked full-image floating-point FrameBuffer allocation.
    let mut row = vec![0f32; info.width as usize * channels];
    for target in rgba.chunks_exact_mut(info.width as usize * 4) {
        ensure!(
            stream.write_to_buffer(&mut row) == row.len(),
            "Truncated JPEG XL rendered pixels"
        );
        for (src, dst) in row.chunks_exact(channels).zip(target.chunks_exact_mut(4)) {
            ensure!(
                src.iter().all(|s| s.is_finite()),
                "Non-finite JPEG XL pixels"
            );
            for c in 0..3 {
                dst[c] = quantize(src[c]);
            }
            dst[3] = if channels == 4 { quantize(src[3]) } else { 255 };
        }
    }
    let rgba = RgbaImage::from_raw(info.width, info.height, rgba)
        .context("Invalid JPEG XL output length")?;
    // Release decoder grids before image's resize scratch is allocated.
    drop(stream);
    drop(rendered);
    drop(decoder);
    let mut image = DynamicImage::ImageRgba8(rgba);
    if info.width.max(info.height) > max_edge {
        image = crate::media::resize_triangle_edge(&image, max_edge);
    }
    Ok(DecodedJxl {
        image,
        icc,
        converted_to_srgb: true,
        source_width: info.width,
        source_height: info.height,
    })
}

fn quantize(sample: f32) -> u8 {
    (sample.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unmodified codestream from libjxl/testdata's square fixture, CC BY 4.0,
    // JPEG XL Project authors. Native libjxl 0.11 independently decodes it as
    // an 8x8 opaque black image. Container bytes excluded for the raw-path test.
    // https://github.com/libjxl/testdata/blob/73695d303670c90e4d506ea89d9901b081385089/jxl/boxes/square-extended-size-container.jxl
    const BLACK_SQUARE: &[u8] = &[
        0xff, 0x0a, 0x41, 0x06, 0x00, 0x12, 0x88, 0x02, 0x00, 0xb4, 0x00, 0x55, 0x0f, 0x00, 0x00,
        0xa8, 0x50, 0x19, 0x65, 0xdc, 0xe0, 0xe5, 0x5c, 0xcf, 0x97, 0x1f, 0x3a, 0x2c, 0xa6, 0x6d,
        0x5c, 0x67, 0x68, 0xab, 0x6d, 0x0b, 0x4b, 0x12, 0x45, 0xc6, 0xb1, 0x49, 0x3a, 0x81, 0x43,
        0x92, 0x48, 0xd2, 0x60, 0x20, 0x99, 0xc1, 0x70, 0x5a, 0x25, 0x09,
    ];

    // Generated from six synthetic RGBA pixels with upstream libjxl 0.11.1,
    // lossless, EXIF orientation 6; dedicated to CC0. Native decoder confirms
    // these oriented samples, including the RGB value behind zero alpha.
    const ROTATED_RGBA: &[u8] = &[
        0xff, 0x0a, 0x10, 0x00, 0x02, 0x58, 0x60, 0x4d, 0x08, 0x10, 0x10, 0x00, 0x98, 0x00, 0x4b,
        0x18, 0x8b, 0x15, 0xc2, 0x11, 0xe8, 0x6d, 0x50, 0x3e, 0x17, 0x90, 0x02, 0x00, 0xf4, 0xb7,
        0xff, 0xad, 0xa0, 0x88, 0x97, 0x0a, 0x01, 0xf6, 0xeb, 0x69, 0x7f, 0x00, 0xf1, 0xc6, 0x7c,
        0xb7, 0xfe, 0x43, 0xf5, 0xc5, 0xa0, 0x14,
    ];

    #[test]
    fn orientation_is_applied_once_and_straight_alpha_is_preserved() {
        let info = inspect(ROTATED_RGBA).unwrap();
        assert_eq!(
            (
                info.coded_width,
                info.coded_height,
                info.width,
                info.height,
                info.orientation
            ),
            (2, 3, 3, 2, 6)
        );
        assert!(info.has_alpha && !info.associated_alpha);
        let decoded = decode(ROTATED_RGBA, 10, working_bytes(ROTATED_RGBA, 10).unwrap()).unwrap();
        assert_eq!(
            decoded.image.to_rgba8().into_raw(),
            [
                0, 255, 255, 128, 0, 0, 255, 64, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 255,
                0, 255, 0, 128,
            ]
        );
    }

    #[test]
    fn file_inspection_seeks_over_large_auxiliary_boxes_and_checks_source_limit() {
        use std::io::Write;
        let mut raw = tempfile::NamedTempFile::new().unwrap();
        raw.write_all(ROTATED_RGBA).unwrap();
        let info = inspect_file(raw.path()).unwrap();
        assert_eq!((info.width, info.height, info.orientation), (3, 2, 6));

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a").unwrap();
        // Sparse 32 MiB auxiliary payload. Its bytes are never needed to index
        // the tiny image following it, and invalid brob bytes must not inflate.
        let aux_len = 32 * 1024 * 1024u32;
        file.write_all(&(aux_len + 8).to_be_bytes()).unwrap();
        file.write_all(b"brobBAD!").unwrap();
        file.seek(SeekFrom::Start(12 + 8 + u64::from(aux_len)))
            .unwrap();
        file.write_all(&(8 + BLACK_SQUARE.len() as u32).to_be_bytes())
            .unwrap();
        file.write_all(b"jxlc").unwrap();
        file.write_all(BLACK_SQUARE).unwrap();
        let info = inspect_file(file.path()).unwrap();
        assert_eq!((info.width, info.height), (8, 8));
        file.as_file_mut().set_len(MAX_SOURCE as u64 + 1).unwrap();
        assert!(
            inspect_file(file.path())
                .unwrap_err()
                .to_string()
                .contains("512 MiB")
        );
    }

    #[test]
    fn official_square_has_correct_pixels_and_obeys_admission() {
        let info = inspect(BLACK_SQUARE).unwrap();
        assert_eq!(
            (info.width, info.height, info.orientation, info.bit_depth),
            (8, 8, 1, 8)
        );
        assert!(!info.has_alpha && !info.animated && !info.hdr);
        let budget = working_bytes(BLACK_SQUARE, 8).unwrap();
        assert!(decode(BLACK_SQUARE, 8, budget - 1).is_err());
        let decoded = decode(BLACK_SQUARE, 8, budget).unwrap();
        assert!(
            decoded
                .image
                .to_rgba8()
                .pixels()
                .all(|p| p.0 == [0, 0, 0, 255])
        );
        let thumbnail = decode(BLACK_SQUARE, 3, budget).unwrap();
        assert_eq!((thumbnail.image.width(), thumbnail.image.height()), (3, 3));
        assert_eq!((thumbnail.source_width, thumbnail.source_height), (8, 8));
        assert!(decode(&BLACK_SQUARE[..BLACK_SQUARE.len() - 3], 8, budget).is_err());
    }

    #[test]
    fn rejects_truncated_and_forged_container_sizes() {
        for bytes in [&[][..], &[0xff][..], &[0xff, 0x0a][..], b"not jxl"] {
            assert!(inspect(bytes).is_err());
        }
        let mut bytes = b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a".to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"jxlc");
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(inspect(&bytes).is_err());
    }

    #[test]
    fn auxiliary_brotli_payload_is_never_decompressed() {
        let mut bytes = b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a".to_vec();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(b"brobExifBAD!");
        bytes.extend_from_slice(&10u32.to_be_bytes());
        bytes.extend_from_slice(b"jxlc\xff\x0a");
        assert_eq!(codestream(&bytes).unwrap(), vec![&[0xff, 0x0a][..]]);
        assert!(decode(&bytes, 32, usize::MAX).is_err());
    }

    #[test]
    fn multipart_requires_order_and_final_marker() {
        let mut bytes = b"\0\0\0\x0cJXL \x0d\x0a\x87\x0a".to_vec();
        bytes.extend_from_slice(&14u32.to_be_bytes());
        bytes.extend_from_slice(b"jxlp");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&[0xff, 0x0a]);
        assert!(codestream(&bytes).is_err());
        bytes[20..24].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert_eq!(codestream(&bytes).unwrap(), vec![&[0xff, 0x0a][..]]);
        bytes[20..24].copy_from_slice(&0x8000_0001u32.to_be_bytes());
        assert!(codestream(&bytes).is_err());
    }

    #[test]
    fn icc_lengths_reject_overflow_and_truncation() {
        assert!(icc_varint(&mut &[0x80][..]).is_err());
        assert!(icc_varint(&mut &[0xff; 9][..]).is_err());
        assert_eq!(
            icc_varint(&mut &[0x80, 0x80, 0x80, 2][..]).unwrap(),
            4 * 1024 * 1024
        );
    }

    #[test]
    #[ignore = "opt-in official libjxl/testdata fixtures; set SHADOWROOM_JXL_FIXTURE_DIR"]
    fn official_fixtures_decode_and_reject_hdr() {
        let dir = std::env::var_os("SHADOWROOM_JXL_FIXTURE_DIR")
            .expect("set SHADOWROOM_JXL_FIXTURE_DIR to formats/jxl-fixtures/jxl");
        let dir = Path::new(&dir);
        for name in [
            "boxes/square-extended-size-container.jxl",
            "splines.jxl",
            "jpeg_reconstruction/1x1_exif_xmp.jxl",
            "blending/cropped_traffic_light.jxl",
        ] {
            let bytes = std::fs::read(dir.join(name)).unwrap();
            let info = inspect(&bytes).unwrap();
            let budget = working_bytes(&bytes, 128).unwrap();
            assert!(
                decode(&bytes, 128, budget - 1).is_err(),
                "admission: {name}"
            );
            let decoded = decode(&bytes, 128, budget).unwrap();
            assert!(decoded.converted_to_srgb);
            assert_eq!(
                (decoded.source_width, decoded.source_height),
                (info.width, info.height)
            );
            assert!(decoded.image.width().max(decoded.image.height()) <= 128);
            assert!(
                decoded.image.to_rgba8().pixels().any(|p| p.0[3] > 0),
                "visible pixels: {name}"
            );
            if name.contains("1x1") {
                assert_eq!((info.width, info.height), (1, 1));
            }
            if name.contains("traffic_light") {
                assert!(info.animated && info.first_frame_only);
                assert_eq!((info.width, info.height), (50, 80));
                let rgba = decoded.image.to_rgba8();
                assert!(
                    rgba.pixels()
                        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
                        .count()
                        > 400
                );
                assert!(
                    !rgba
                        .pixels()
                        .any(|p| p[1] > 150 && p[0] < 100 && p[2] < 100)
                );
            }
            assert!(
                decode(&bytes[..16.min(bytes.len() / 2)], 128, budget).is_err(),
                "truncated first frame: {name}"
            );
        }
        let hdr = std::fs::read(dir.join("pq_gradient.jxl")).unwrap();
        assert!(inspect(&hdr).unwrap().hdr);
        assert!(
            decode(&hdr, 128, working_bytes(&hdr, 128).unwrap())
                .unwrap_err_string()
                .contains("tone mapping")
        );

        let generated = dir.parent().unwrap().join("generated");
        // Expected sample orders independently verified with native libjxl.
        let source = [
            [255, 0, 0, 255],
            [0, 255, 0, 128],
            [0, 0, 255, 64],
            [255, 255, 0, 255],
            [0, 255, 255, 128],
            [255, 0, 255, 0],
        ];
        let orders = [
            [0, 1, 2, 3, 4, 5],
            [1, 0, 3, 2, 5, 4],
            [5, 4, 3, 2, 1, 0],
            [4, 5, 2, 3, 0, 1],
            [0, 2, 4, 1, 3, 5],
            [4, 2, 0, 5, 3, 1],
            [5, 3, 1, 4, 2, 0],
            [1, 3, 5, 0, 2, 4],
        ];
        for (i, order) in orders.iter().enumerate() {
            let bytes =
                std::fs::read(generated.join(format!("rgba-orientation-{}.jxl", i + 1))).unwrap();
            let info = inspect(&bytes).unwrap();
            assert_eq!(info.orientation as usize, i + 1);
            let decoded = decode(&bytes, 128, working_bytes(&bytes, 128).unwrap()).unwrap();
            let expected: Vec<u8> = order.iter().flat_map(|&n| source[n]).collect();
            assert_eq!(
                decoded.image.to_rgba8().into_raw(),
                expected,
                "orientation {}",
                i + 1
            );
        }
        let bytes = std::fs::read(generated.join("icc-srgb.jxl")).unwrap();
        assert!(inspect(&bytes).unwrap().has_icc);
        let decoded = decode(&bytes, 128, working_bytes(&bytes, 128).unwrap()).unwrap();
        assert!(decoded.converted_to_srgb && decoded.icc.is_some());
        assert_eq!(
            decoded.image.to_rgba8().into_raw(),
            source.into_iter().flatten().collect::<Vec<_>>()
        );
        let bytes = std::fs::read(generated.join("rgb16-srgb.jxl")).unwrap();
        assert_eq!(inspect(&bytes).unwrap().bit_depth, 16);
        let decoded = decode(&bytes, 128, working_bytes(&bytes, 128).unwrap()).unwrap();
        assert_eq!(
            decoded.image.to_rgba8().into_raw(),
            [
                255, 0, 0, 255, 0, 128, 255, 255, 4, 8, 12, 255, 39, 78, 117, 255, 255, 255, 255,
                255, 0, 0, 0, 255,
            ]
        );
        let bytes = std::fs::read(generated.join("premultiplied.jxl")).unwrap();
        assert!(inspect(&bytes).unwrap().associated_alpha);
        assert!(
            decode(&bytes, 128, working_bytes(&bytes, 128).unwrap())
                .unwrap_err_string()
                .contains("associated-alpha")
        );
    }

    trait ErrorString {
        fn unwrap_err_string(self) -> String;
    }
    impl ErrorString for Result<DecodedJxl> {
        fn unwrap_err_string(self) -> String {
            match self {
                Ok(_) => panic!("expected failure"),
                Err(e) => e.to_string(),
            }
        }
    }
}
