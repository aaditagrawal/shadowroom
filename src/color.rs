//! ICC conversion implemented in Rust. Samples stay at their decoded precision
//! until display quantization, and alpha never enters the color transform.
//!
//! `native-color` keeps the established LittleCMS implementation (the default).
//! The experimental Rust engine differs in CLUT interpolation for some CMYK
//! profiles. It must be selected with defaults disabled, rather than replacing
//! production color output until that compatibility gap is resolved.
#[cfg(not(feature = "native-color"))]
use anyhow::bail;
use anyhow::{Context, Result};
use image::DynamicImage;
#[cfg(not(feature = "native-color"))]
use moxcms::{
    ColorProfile, DataColorSpace, InterpolationMethod, Layout, ProfileClass, RenderingIntent,
    TransformOptions,
};

#[cfg(not(feature = "native-color"))]
pub(crate) fn transform_options() -> TransformOptions {
    TransformOptions {
        rendering_intent: RenderingIntent::RelativeColorimetric,
        prefer_fixed_point: false,
        interpolation_method: InterpolationMethod::Tetrahedral,
        ..Default::default()
    }
}

/// ICC v2 permits a single AToB0 table for all input rendering intents.
/// LittleCMS falls back to it when AToB1 is absent. moxcms selects tables
/// strictly, so provide that same fallback before requesting relative intent.
#[cfg(not(feature = "native-color"))]
pub(crate) fn parse_profile(icc: &[u8]) -> Result<ColorProfile> {
    let mut profile =
        ColorProfile::new_from_slice(icc).context("Cannot parse embedded ICC profile")?;
    if profile.lut_a_to_b_colorimetric.is_none() {
        profile.lut_a_to_b_colorimetric = profile.lut_a_to_b_perceptual.clone();
    }
    Ok(profile)
}

#[cfg(not(feature = "native-color"))]
macro_rules! convert_depth {
    ($name:ident, $sample:ty, $create:ident, $rgb:ident, $rgba:ident, $to_rgb:ident, $to_rgba:ident, $to_luma:ident) => {
        fn $name(image: &mut DynamicImage, source: &ColorProfile) -> Result<()> {
            let gray = source.color_space == DataColorSpace::Gray;
            let transform = source
                .$create(
                    if gray { Layout::Gray } else { Layout::Rgb },
                    &ColorProfile::new_srgb(),
                    Layout::Rgb,
                    transform_options(),
                )
                .context("Embedded profile is incompatible with decoded pixels")?;
            if gray {
                let input = image.$to_luma();
                let mut rgb = image.$to_rgb();
                transform
                    .transform(input.as_raw(), rgb.as_mut())
                    .context("Cannot convert embedded grayscale ICC profile")?;
                if image.color().has_alpha() {
                    let mut rgba = image.$to_rgba();
                    for (pixel, rgb) in rgba.pixels_mut().zip(rgb.pixels()) {
                        pixel.0[..3].copy_from_slice(&rgb.0);
                    }
                    *image = DynamicImage::$rgba(rgba);
                } else {
                    *image = DynamicImage::$rgb(rgb);
                }
                return Ok(());
            }
            // Convert native RGB/RGBA allocations in bounded blocks; leave each
            // alpha sample untouched, including fully transparent hidden color.
            let (raw, channels): (&mut [$sample], usize) = match image {
                DynamicImage::$rgb(rgb) => (rgb.as_mut(), 3),
                DynamicImage::$rgba(rgba) => (rgba.as_mut(), 4),
                _ => {
                    *image = if image.color().has_alpha() {
                        DynamicImage::$rgba(image.$to_rgba())
                    } else {
                        DynamicImage::$rgb(image.$to_rgb())
                    };
                    return $name(image, source);
                }
            };
            let mut input = [0 as $sample; 256 * 3];
            let mut output = [0 as $sample; 256 * 3];
            for block in raw.chunks_mut(256 * channels) {
                let count = block.len() / channels;
                for (pixel, rgb) in block.chunks_exact(channels).zip(input.chunks_exact_mut(3)) {
                    rgb.copy_from_slice(&pixel[..3]);
                }
                transform
                    .transform(&input[..count * 3], &mut output[..count * 3])
                    .context("Cannot convert embedded RGB ICC profile")?;
                for (pixel, rgb) in block.chunks_exact_mut(channels).zip(output.chunks_exact(3)) {
                    pixel[..3].copy_from_slice(rgb);
                }
            }
            Ok(())
        }
    };
}

#[cfg(not(feature = "native-color"))]
convert_depth!(
    convert_u8,
    u8,
    create_transform_8bit,
    ImageRgb8,
    ImageRgba8,
    to_rgb8,
    to_rgba8,
    to_luma8
);
#[cfg(not(feature = "native-color"))]
convert_depth!(
    convert_u16,
    u16,
    create_transform_16bit,
    ImageRgb16,
    ImageRgba16,
    to_rgb16,
    to_rgba16,
    to_luma16
);
#[cfg(not(feature = "native-color"))]
convert_depth!(
    convert_f32,
    f32,
    create_transform_f32,
    ImageRgb32F,
    ImageRgba32F,
    to_rgb32f,
    to_rgba32f,
    to_luma32f
);

#[cfg(not(feature = "native-color"))]
pub(crate) fn convert_image(image: &mut DynamicImage, icc: &[u8]) -> Result<()> {
    let source = parse_profile(icc)?;
    if !matches!(
        source.color_space,
        DataColorSpace::Gray | DataColorSpace::Rgb
    ) || source.profile_class == ProfileClass::DeviceLink
    {
        bail!("Embedded profile is incompatible with decoded RGB pixels");
    }
    match image {
        DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_)
        | DynamicImage::ImageRgb16(_)
        | DynamicImage::ImageRgba16(_) => convert_u16(image, &source),
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => convert_f32(image, &source),
        _ => convert_u8(image, &source),
    }
}

#[cfg(feature = "native-color")]
pub(crate) fn convert_image(image: &mut DynamicImage, icc: &[u8]) -> Result<()> {
    use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Profile, Transform};
    let source = Profile::new_icc(icc).context("Cannot parse embedded ICC profile")?;
    let target = Profile::new_srgb();
    let grayscale = source.color_space() == ColorSpaceSignature::GrayData;
    if grayscale
        && matches!(
            image,
            DynamicImage::ImageLuma16(_)
                | DynamicImage::ImageLumaA16(_)
                | DynamicImage::ImageRgb16(_)
                | DynamicImage::ImageRgba16(_)
        )
    {
        let transform = Transform::<u16, [u16; 3]>::new_flags(
            &source,
            PixelFormat::GRAY_16,
            &target,
            PixelFormat::RGB_16,
            Intent::RelativeColorimetric,
            // LCMS's optimized 16-bit gray LUT rounds away dark tones.
            // Retain its analytic curve pipeline until final quantization.
            lcms2::Flags::NO_OPTIMIZE,
        )
        .context("Embedded grayscale profile is incompatible with decoded pixels")?;
        let alpha = image.color().has_alpha().then(|| {
            image
                .to_rgba16()
                .pixels()
                .map(|p| p[3])
                .collect::<Vec<u16>>()
        });
        let mut rgb =
            image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(image.width(), image.height());
        transform.transform_pixels(
            image.to_luma16().as_raw(),
            rgb.as_mut().as_chunks_mut::<3>().0,
        );
        if let Some(alpha) = alpha {
            let mut rgba = DynamicImage::ImageRgb16(rgb).into_rgba16();
            for (pixel, alpha) in rgba.pixels_mut().zip(alpha) {
                pixel[3] = alpha;
            }
            *image = DynamicImage::ImageRgba16(rgba);
        } else {
            *image = DynamicImage::ImageRgb16(rgb);
        }
        return Ok(());
    }
    if !grayscale {
        match image {
            DynamicImage::ImageRgb16(rgb) => {
                let transform = Transform::<[u16; 3], [u16; 3]>::new(
                    &source,
                    PixelFormat::RGB_16,
                    &target,
                    PixelFormat::RGB_16,
                    Intent::RelativeColorimetric,
                )
                .context("Embedded profile is incompatible with decoded RGB pixels")?;
                transform.transform_in_place(rgb.as_mut().as_chunks_mut::<3>().0);
                return Ok(());
            }
            DynamicImage::ImageRgba16(rgba) => {
                let transform = Transform::<[u16; 4], [u16; 4]>::new_flags(
                    &source,
                    PixelFormat::RGBA_16,
                    &target,
                    PixelFormat::RGBA_16,
                    Intent::RelativeColorimetric,
                    lcms2::Flags::COPY_ALPHA,
                )
                .context("Embedded profile is incompatible with decoded RGB pixels")?;
                transform.transform_in_place(rgba.as_mut().as_chunks_mut::<4>().0);
                return Ok(());
            }
            _ => (),
        }
    }
    if !grayscale {
        let format = match image {
            DynamicImage::ImageRgb8(_) => Some(PixelFormat::RGB_8),
            DynamicImage::ImageRgba8(_) => Some(PixelFormat::RGBA_8),
            _ => None,
        };
        if let Some(format) = format {
            let transform = Transform::<u8, u8>::new_flags(
                &source,
                format,
                &target,
                format,
                Intent::RelativeColorimetric,
                if format == PixelFormat::RGBA_8 {
                    lcms2::Flags::COPY_ALPHA
                } else {
                    lcms2::Flags::default()
                },
            )
            .context("Embedded profile is incompatible with decoded RGB pixels")?;
            match image {
                DynamicImage::ImageRgb8(rgb) => transform.transform_in_place(rgb.as_mut()),
                DynamicImage::ImageRgba8(rgba) => transform.transform_in_place(rgba.as_mut()),
                _ => unreachable!(),
            }
            return Ok(());
        }
    }
    let transform = Transform::<u8, u8>::new(
        &source,
        if grayscale {
            PixelFormat::GRAY_8
        } else {
            PixelFormat::RGB_8
        },
        &target,
        PixelFormat::RGB_8,
        Intent::RelativeColorimetric,
    )
    .context("Embedded profile is incompatible with decoded RGB pixels")?;
    let alpha = image
        .color()
        .has_alpha()
        .then(|| image.to_rgba8().pixels().map(|p| p[3]).collect::<Vec<u8>>());
    let mut rgb = image.to_rgb8();
    if grayscale {
        transform.transform_pixels(image.to_luma8().as_raw(), rgb.as_mut());
    } else {
        transform.transform_in_place(rgb.as_mut());
    }
    if let Some(alpha) = alpha {
        let mut rgba = DynamicImage::ImageRgb8(rgb).to_rgba8();
        for (pixel, a) in rgba.pixels_mut().zip(alpha) {
            pixel[3] = a;
        }
        *image = DynamicImage::ImageRgba8(rgba);
    } else {
        *image = DynamicImage::ImageRgb8(rgb);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, LumaA};
    use lcms2::{Flags, Intent, PixelFormat, Profile, ToneCurve, Transform};

    #[test]
    fn gray_curves_keep_native_sixteen_bit_precision_and_straight_alpha() {
        for gamma in [1.0, 1.8, 2.2] {
            let profile = Profile::new_gray(&lcms2::CIExyY::d50(), &ToneCurve::new(gamma)).unwrap();
            let icc = profile.icc().unwrap();
            // ICC serialization quantizes gamma; compare the same stored curve,
            // rather than the higher precision fixture-construction parameter.
            let profile = Profile::new_icc(&icc).unwrap();
            let input: Vec<u16> = (0..=65535).collect();
            let mut reference = vec![[0u16; 3]; input.len()];
            Transform::<u16, [u16; 3]>::new_flags(
                &profile,
                PixelFormat::GRAY_16,
                &Profile::new_srgb(),
                PixelFormat::RGB_16,
                Intent::RelativeColorimetric,
                Flags::NO_OPTIMIZE,
            )
            .unwrap()
            .transform_pixels(&input, &mut reference);
            let mut image = DynamicImage::ImageLumaA16(ImageBuffer::from_fn(256, 256, |x, y| {
                let value = (y * 256 + x) as u16;
                LumaA([value, value ^ 0x55aa])
            }));
            convert_image(&mut image, &icc).unwrap();
            let actual = image.to_rgba16();
            let mut max_display_error = 0;
            for (pixel, (gray, expected)) in actual.pixels().zip(input.iter().zip(reference)) {
                assert_eq!(
                    pixel[3],
                    gray ^ 0x55aa,
                    "gamma {gamma}: alpha must be untouched"
                );
                for (actual, expected) in pixel.0[..3].iter().zip(expected) {
                    // Both engines carry u16 data through conversion. Permit
                    // adjacent final u8 codes, never an early u8 input reduction.
                    let quantize = |v: u16| ((u32::from(v) * 255 + 32767) / 65535) as u8;
                    max_display_error =
                        max_display_error.max(quantize(*actual).abs_diff(quantize(expected)));
                }
            }
            let tolerance = if cfg!(feature = "native-color") { 0 } else { 1 };
            assert!(
                max_display_error <= tolerance,
                "gamma {gamma}: maximum display error {max_display_error}"
            );
            if gamma == 1.0 {
                assert!((1000..=1400).contains(&actual.get_pixel(100, 0)[0]));
            }
        }
    }

    #[cfg(not(feature = "native-color"))]
    #[test]
    fn float_linear_rgb_is_not_quantized_and_preserves_straight_alpha() {
        let gamma = ToneCurve::new(1.0);
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
        let white = lcms2::CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let icc = Profile::new_rgb(&white, &primaries, &[&gamma, &gamma, &gamma])
            .unwrap()
            .icc()
            .unwrap();
        let samples = [0.0015f32, 0.003, 0.5, 0.1234567];
        let mut image =
            DynamicImage::ImageRgba32F(ImageBuffer::from_pixel(1, 1, image::Rgba(samples)));
        convert_image(&mut image, &icc).unwrap();
        let pixel = image.as_rgba32f().unwrap().get_pixel(0, 0);
        for channel in 0..3 {
            let linear = samples[channel];
            let encoded = if linear <= 0.0031308 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            assert!(
                (pixel[channel] - encoded).abs() < 0.0002,
                "{pixel:?}, expected {encoded}"
            );
        }
        assert_eq!(pixel[3].to_bits(), samples[3].to_bits());
    }
}
