//! Georeferenced display rasters used as triangulation surface textures.

#[cfg(not(target_arch = "wasm32"))]
use std::io::BufReader;
#[cfg(target_arch = "wasm32")]
use std::io::Cursor;
use std::{
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use tiff::{
    ColorType,
    decoder::{Decoder, DecodingResult},
    tags::Tag,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RasterTextureId(pub(crate) u64);

/// CPU-side raster prepared by a background decoder. Large images are normally
/// bounded by the raster-preview performance preference.
pub(crate) struct LoadedRasterTexture {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) source_size: [u32; 2],
    pub(crate) preview_size: [u32; 2],
    pub(crate) rgba: Arc<Vec<u8>>,
    /// Affine map from world XY to normalized texture UV:
    /// `u = a*x + b*y + c`, `v = d*x + e*y + f`.
    pub(crate) world_to_uv: [f64; 6],
    pub(crate) projection: String,
    pub(crate) driver_name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenRasterTexture {
    pub(crate) id: RasterTextureId,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) source_size: [u32; 2],
    pub(crate) preview_size: [u32; 2],
    pub(crate) rgba: Arc<Vec<u8>>,
    pub(crate) world_to_uv: [f64; 6],
    pub(crate) projection: String,
    pub(crate) driver_name: String,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_supported_raster_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "tif" | "tiff"))
}

fn world_to_uv_transform(transform: [f64; 6], size: [u32; 2]) -> Result<[f64; 6]> {
    let determinant = transform[1] * transform[5] - transform[2] * transform[4];
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        anyhow::bail!("Raster has a singular geotransform");
    }
    let width = f64::from(size[0]);
    let height = f64::from(size[1]);
    let a = transform[5] / determinant / width;
    let b = -transform[2] / determinant / width;
    let d = -transform[4] / determinant / height;
    let e = transform[1] / determinant / height;
    let c = -(a * transform[0] + b * transform[3]);
    let f = -(d * transform[0] + e * transform[3]);
    Ok([a, b, c, d, e, f])
}

/// GDAL-style geotransform `[x0, dx_col, dx_row, y0, dy_col, dy_row]` from the
/// GeoTIFF tags: a full ModelTransformation matrix when present, otherwise the
/// common PixelScale + Tiepoint (north-up) pair.
fn read_geotransform<R: Read + Seek>(decoder: &mut Decoder<R>) -> Result<[f64; 6]> {
    if let Ok(Some(value)) = decoder.find_tag(Tag::ModelTransformationTag)
        && let Ok(matrix) = value.into_f64_vec()
        && matrix.len() >= 8
    {
        // Row-major 4x4: x = m0*col + m1*row + m3, y = m4*col + m5*row + m7.
        return Ok([matrix[3], matrix[0], matrix[1], matrix[7], matrix[4], matrix[5]]);
    }
    let scale = decoder
        .find_tag(Tag::ModelPixelScaleTag)
        .ok()
        .flatten()
        .and_then(|value| value.into_f64_vec().ok())
        .filter(|scale| scale.len() >= 2);
    let tiepoint = decoder
        .find_tag(Tag::ModelTiepointTag)
        .ok()
        .flatten()
        .and_then(|value| value.into_f64_vec().ok())
        .filter(|tiepoint| tiepoint.len() >= 6);
    if let (Some(scale), Some(tiepoint)) = (scale, tiepoint) {
        // Tiepoint (i, j, k) -> (x, y, z); pixel scale y is positive but rows
        // grow southwards, hence the negated row step.
        let origin_x = tiepoint[3] - tiepoint[0] * scale[0];
        let origin_y = tiepoint[4] + tiepoint[1] * scale[1];
        return Ok([origin_x, scale[0], 0.0, origin_y, 0.0, -scale[1]]);
    }
    anyhow::bail!("Missing ModelTransformation or PixelScale/Tiepoint GeoTIFF tags")
}

/// Human-readable CRS: "EPSG:code" from the GeoKey directory plus the citation
/// text when present. Empty when the file carries no CRS keys.
fn read_projection<R: Read + Seek>(decoder: &mut Decoder<R>) -> String {
    let Ok(directory) = decoder.get_tag_u16_vec(Tag::GeoKeyDirectoryTag) else {
        return String::new();
    };
    let ascii_params = decoder.get_tag_ascii_string(Tag::GeoAsciiParamsTag).unwrap_or_default();

    let mut epsg = None;
    let mut citation = None;
    // Header is [version, revision, minor, key count]; keys follow as
    // [key id, tag location, count, value] quadruples.
    for key in directory.get(4..).unwrap_or_default().as_chunks::<4>().0 {
        let (key_id, location, count, value) = (key[0], key[1], key[2], key[3]);
        match key_id {
            // ProjectedCSType wins over GeographicType when both are present.
            3072 if location == 0 => epsg = Some(value),
            2048 if location == 0 && epsg.is_none() => epsg = Some(value),
            // GTCitation / PCSCitation index into the ascii params tag.
            1026 | 3073 if location == 34737 && citation.is_none() => {
                let start = usize::from(value);
                let end = start.saturating_add(usize::from(count));
                citation = ascii_params
                    .get(start..end.min(ascii_params.len()))
                    .map(|text| text.trim_end_matches(['|', '\0']).trim().to_owned());
            }
            _ => {}
        }
    }

    match (epsg, citation.filter(|text| !text.is_empty())) {
        (Some(epsg), Some(citation)) => format!("EPSG:{epsg} ({citation})"),
        (Some(epsg), None) => format!("EPSG:{epsg}"),
        (None, Some(citation)) => citation,
        (None, None) => String::new(),
    }
}

/// Seek to the smallest reduced-resolution IFD (COG overview) that still meets
/// the preview limit. If every overview is smaller than the target, use the
/// largest reduced overview instead of falling all the way back to the full
/// resolution image.
fn seek_to_best_overview<R: Read + Seek>(decoder: &mut Decoder<R>, limit: u32) -> Result<()> {
    let (width, height) = decoder.dimensions()?;
    if width.max(height) <= limit {
        return Ok(());
    }
    let mut overviews = Vec::new();
    let mut index = 0;
    while decoder.more_images() {
        if decoder.next_image().is_err() {
            break;
        }
        index += 1;
        // NewSubfileType bit 0 marks reduced-resolution overviews; bit 2 marks
        // transparency masks, which are not imagery.
        let subfile_type = decoder.find_tag(Tag::NewSubfileType).ok().flatten().and_then(|value| value.into_u64().ok()).unwrap_or(0);
        if subfile_type & 1 == 0 || subfile_type & 4 != 0 {
            continue;
        }
        let Ok((width, height)) = decoder.dimensions() else {
            continue;
        };
        let max_dimension = width.max(height);
        overviews.push((index, max_dimension));
    }
    decoder.seek_to_image(best_overview_index(limit, &overviews).unwrap_or(0))?;
    Ok(())
}

fn best_overview_index(limit: u32, overviews: &[(usize, u32)]) -> Option<usize> {
    let at_or_above = overviews.iter().filter(|(_, dimension)| *dimension >= limit).min_by_key(|(_, dimension)| *dimension);
    at_or_above
        .or_else(|| overviews.iter().filter(|(_, dimension)| *dimension < limit).max_by_key(|(_, dimension)| *dimension))
        .map(|(index, _)| *index)
}

#[derive(Clone, Copy)]
enum PixelLayout {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
    /// JPEG-in-TIFF; the decoder returns raw YCbCr samples without color
    /// conversion, so BT.601 conversion happens here.
    YCbCr,
}

impl PixelLayout {
    fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayAlpha => 2,
            Self::Rgb | Self::YCbCr => 3,
            Self::Rgba => 4,
        }
    }
}

#[derive(Clone, Copy)]
enum RasterDecodeMode {
    Integer(PixelLayout),
    FloatRgb {
        source_channels: usize,
        rgb_bands: [usize; 3],
        nodata: Option<f32>,
    },
}

fn gdal_band_descriptions<R: Read + Seek>(decoder: &mut Decoder<R>) -> Vec<(usize, String)> {
    let metadata = decoder.get_tag_ascii_string(Tag::Unknown(42112)).unwrap_or_default();
    metadata
        .split("<Item ")
        .filter(|item| item.contains("name=\"DESCRIPTION\""))
        .filter_map(|item| {
            let sample = item.split_once("sample=\"")?.1.split_once('"')?.0.parse().ok()?;
            let description = item.split_once('>')?.1.split_once("</Item>")?.0.trim();
            Some((sample, description.to_ascii_lowercase()))
        })
        .collect()
}

fn rgb_band_indices<R: Read + Seek>(decoder: &mut Decoder<R>, channels: usize) -> [usize; 3] {
    let descriptions = gdal_band_descriptions(decoder);
    let find = |colour: &str| {
        descriptions
            .iter()
            .find(|(_, description)| description == colour || description.ends_with(&format!("_{colour}")) || description.ends_with(&format!("-{colour}")))
            .map(|(sample, _)| *sample)
            .filter(|sample| *sample < channels)
    };
    match (find("red"), find("green"), find("blue")) {
        (Some(red), Some(green), Some(blue)) => [red, green, blue],
        _ => [0, 1.min(channels - 1), 2.min(channels - 1)],
    }
}

fn integer_chunk_samples<R: Read + Seek>(decoder: &mut Decoder<R>, chunk_index: u32, chunks_per_plane: u32, channels: usize, planar: bool) -> Result<Vec<u8>> {
    if planar {
        let pixels = decoder.chunk_data_dimensions(chunk_index);
        let pixels = pixels.0 as usize * pixels.1 as usize;
        let mut planes = Vec::with_capacity(channels);
        for channel in 0..channels {
            let band_chunk = chunk_index + channel as u32 * chunks_per_plane;
            let samples = match decoder.read_chunk(band_chunk)? {
                DecodingResult::U8(samples) => samples,
                DecodingResult::U16(samples) => samples.into_iter().map(|value| (value >> 8) as u8).collect(),
                _ => anyhow::bail!("Raster has an unsupported integer sample format"),
            };
            if samples.len() < pixels {
                anyhow::bail!("Planar raster produced a truncated band chunk");
            }
            planes.push(samples);
        }
        return Ok((0..pixels).flat_map(|pixel| planes.iter().map(move |plane| plane[pixel])).collect());
    }
    match decoder.read_chunk(chunk_index)? {
        DecodingResult::U8(samples) => Ok(samples),
        DecodingResult::U16(samples) => Ok(samples.into_iter().map(|value| (value >> 8) as u8).collect()),
        _ => anyhow::bail!("Raster has an unsupported integer sample format"),
    }
}

fn float_chunk_samples<R: Read + Seek>(decoder: &mut Decoder<R>, chunk_index: u32, chunks_per_plane: u32, channels: usize, planar: bool) -> Result<Vec<f32>> {
    if planar {
        let pixels = decoder.chunk_data_dimensions(chunk_index);
        let pixels = pixels.0 as usize * pixels.1 as usize;
        let mut planes = Vec::with_capacity(channels);
        for channel in 0..channels {
            let band_chunk = chunk_index + channel as u32 * chunks_per_plane;
            let DecodingResult::F32(samples) = decoder.read_chunk(band_chunk)? else {
                anyhow::bail!("Raster has an unsupported floating-point sample format");
            };
            if samples.len() < pixels {
                anyhow::bail!("Planar raster produced a truncated band chunk");
            }
            planes.push(samples);
        }
        return Ok((0..pixels).flat_map(|pixel| planes.iter().map(move |plane| plane[pixel])).collect());
    }
    match decoder.read_chunk(chunk_index)? {
        DecodingResult::F32(samples) => Ok(samples),
        _ => anyhow::bail!("Raster has an unsupported floating-point sample format"),
    }
}

/// Expand one decoded pixel into RGBA bytes; 16-bit samples keep their high byte.
fn chunk_pixel_rgba(samples: &[u8], layout: PixelLayout, pixel: usize) -> [u8; 4] {
    let base = pixel * layout.channels();
    match layout {
        PixelLayout::Gray => [samples[base], samples[base], samples[base], 255],
        PixelLayout::GrayAlpha => [samples[base], samples[base], samples[base], samples[base + 1]],
        PixelLayout::Rgb => [samples[base], samples[base + 1], samples[base + 2], 255],
        PixelLayout::Rgba => [samples[base], samples[base + 1], samples[base + 2], samples[base + 3]],
        PixelLayout::YCbCr => {
            let y = f32::from(samples[base]);
            let cb = f32::from(samples[base + 1]) - 128.0;
            let cr = f32::from(samples[base + 2]) - 128.0;
            let clamp = |value: f32| value.round().clamp(0.0, 255.0) as u8;
            [clamp(y + 1.402 * cr), clamp(y - 0.344_136 * cb - 0.714_136 * cr), clamp(y + 1.772 * cb), 255]
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn decode_raster(path: &Path, max_preview_dimension: u32) -> Result<LoadedRasterTexture> {
    let file = std::fs::File::open(path).with_context(|| format!("Failed to open raster {}", path.display()))?;
    decode_raster_reader(BufReader::new(file), path, max_preview_dimension)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn decode_raster_bytes(source_name: &str, bytes: &[u8], max_preview_dimension: u32) -> Result<LoadedRasterTexture> {
    decode_raster_reader(Cursor::new(bytes), Path::new(source_name), max_preview_dimension)
}

fn decode_raster_reader<R: Read + Seek>(reader: R, path: &Path, max_preview_dimension: u32) -> Result<LoadedRasterTexture> {
    let mut decoder = Decoder::new(reader).with_context(|| format!("Failed to read raster {}", path.display()))?;

    let (source_width, source_height) = decoder.dimensions()?;
    if source_width == 0 || source_height == 0 {
        anyhow::bail!("Raster {} has no pixels", path.display());
    }

    // Georeferencing always comes from the full-resolution IFD; overviews span
    // the same extent so the normalized UV transform is shared.
    let transform = read_geotransform(&mut decoder).with_context(|| format!("Raster {} has no usable geotransform", path.display()))?;
    let world_to_uv = world_to_uv_transform(transform, [source_width, source_height]).with_context(|| format!("Raster {} has an invalid geotransform", path.display()))?;
    let projection = read_projection(&mut decoder);

    let limit = max_preview_dimension.max(1);
    seek_to_best_overview(&mut decoder, limit)?;
    let (decode_width, decode_height) = decoder.dimensions()?;

    let planar = decoder.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    let mode = match decoder.colortype()? {
        ColorType::Gray(8 | 16) => RasterDecodeMode::Integer(PixelLayout::Gray),
        ColorType::GrayA(8 | 16) => RasterDecodeMode::Integer(PixelLayout::GrayAlpha),
        ColorType::RGB(8 | 16) => RasterDecodeMode::Integer(PixelLayout::Rgb),
        ColorType::RGBA(8 | 16) => RasterDecodeMode::Integer(PixelLayout::Rgba),
        ColorType::YCbCr(8) => RasterDecodeMode::Integer(PixelLayout::YCbCr),
        ColorType::Multiband { bit_depth: 32, num_samples }
            if num_samples >= 3 && decoder.get_tag_u16_vec(Tag::SampleFormat).is_ok_and(|formats| formats.iter().all(|format| *format == 3)) =>
        {
            let source_channels = usize::from(num_samples);
            let rgb_bands = rgb_band_indices(&mut decoder, source_channels);
            let nodata = decoder
                .get_tag_ascii_string(Tag::GdalNodata)
                .ok()
                .and_then(|value| value.trim_matches('\0').trim().parse().ok());
            RasterDecodeMode::FloatRgb {
                source_channels,
                rgb_bands,
                nodata,
            }
        }
        other => anyhow::bail!("Raster {} has unsupported pixel format {other:?}", path.display()),
    };

    let scale = (f64::from(limit) / f64::from(decode_width.max(decode_height))).min(1.0);
    let preview_width = ((f64::from(decode_width) * scale).round() as u32).max(1);
    let preview_height = ((f64::from(decode_height) * scale).round() as u32).max(1);

    // Chunks (strips or tiles) are streamed and box-filtered into the preview
    // so the full-resolution image is never held in memory at once.
    let preview_pixels = preview_width as usize * preview_height as usize;
    // RGB is accumulated premultiplied by alpha. Averaging straight RGB would
    // let the colour of fully transparent texels bleed into visible edges.
    let mut sums = vec![0u64; preview_pixels * 4];
    let mut counts = vec![0u64; preview_pixels];
    let mut float_sums = vec![[0.0f64; 3]; preview_pixels];

    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    if chunk_width == 0 || chunk_height == 0 {
        anyhow::bail!("Raster {} has an invalid chunk layout", path.display());
    }
    let chunks_across = decode_width.div_ceil(chunk_width);
    let chunk_count = chunks_across * decode_height.div_ceil(chunk_height);
    for chunk_index in 0..chunk_count {
        let (data_width, data_height) = decoder.chunk_data_dimensions(chunk_index);
        let integer_samples = match mode {
            RasterDecodeMode::Integer(layout) => Some(
                integer_chunk_samples(&mut decoder, chunk_index, chunk_count, layout.channels(), planar).with_context(|| format!("Failed to decode raster {}", path.display()))?,
            ),
            RasterDecodeMode::FloatRgb { .. } => None,
        };
        let float_samples = match mode {
            RasterDecodeMode::FloatRgb { source_channels, .. } => {
                Some(float_chunk_samples(&mut decoder, chunk_index, chunk_count, source_channels, planar).with_context(|| format!("Failed to decode raster {}", path.display()))?)
            }
            RasterDecodeMode::Integer(_) => None,
        };
        let x0 = (chunk_index % chunks_across) * chunk_width;
        let y0 = (chunk_index / chunks_across) * chunk_height;
        for row in 0..data_height {
            let py = ((u64::from(y0 + row) * u64::from(preview_height)) / u64::from(decode_height)) as usize;
            for col in 0..data_width {
                let px = ((u64::from(x0 + col) * u64::from(preview_width)) / u64::from(decode_width)) as usize;
                let source_pixel = (row * data_width + col) as usize;
                let target = py * preview_width as usize + px;
                match mode {
                    RasterDecodeMode::Integer(layout) => {
                        let samples = integer_samples.as_ref().unwrap();
                        let expected = (source_pixel + 1) * layout.channels();
                        if samples.len() < expected {
                            anyhow::bail!("Raster {} produced a truncated chunk", path.display());
                        }
                        let rgba = chunk_pixel_rgba(samples, layout, source_pixel);
                        let alpha = u64::from(rgba[3]);
                        sums[target * 4] += u64::from(rgba[0]) * alpha;
                        sums[target * 4 + 1] += u64::from(rgba[1]) * alpha;
                        sums[target * 4 + 2] += u64::from(rgba[2]) * alpha;
                        sums[target * 4 + 3] += alpha;
                        counts[target] += 1;
                    }
                    RasterDecodeMode::FloatRgb {
                        source_channels,
                        rgb_bands,
                        nodata,
                    } => {
                        let samples = float_samples.as_ref().unwrap();
                        let base = source_pixel * source_channels;
                        if samples.len() < base + source_channels {
                            anyhow::bail!("Raster {} produced a truncated chunk", path.display());
                        }
                        let rgb = rgb_bands.map(|band| samples[base + band]);
                        if rgb.iter().all(|value| value.is_finite()) && !nodata.is_some_and(|nodata| rgb.contains(&nodata)) {
                            for channel in 0..3 {
                                float_sums[target][channel] += f64::from(rgb[channel]);
                            }
                            counts[target] += 1;
                        }
                    }
                }
            }
        }
    }

    let rgba = match mode {
        RasterDecodeMode::Integer(_) => {
            let mut rgba = Vec::with_capacity(preview_pixels * 4);
            for (pixel, count) in counts.into_iter().enumerate() {
                rgba.extend_from_slice(&finish_premultiplied_average(
                    [sums[pixel * 4], sums[pixel * 4 + 1], sums[pixel * 4 + 2], sums[pixel * 4 + 3]],
                    count,
                ));
            }
            rgba
        }
        RasterDecodeMode::FloatRgb { .. } => finish_float_rgb(float_sums, &counts),
    };

    Ok(LoadedRasterTexture {
        name: path.file_name().and_then(|value| value.to_str()).unwrap_or("Raster").to_owned(),
        path: path.to_owned(),
        source_size: [source_width, source_height],
        preview_size: [preview_width, preview_height],
        rgba: Arc::new(rgba),
        world_to_uv,
        projection,
        driver_name: "GeoTIFF".to_owned(),
    })
}

fn finish_float_rgb(sums: Vec<[f64; 3]>, counts: &[u64]) -> Vec<u8> {
    let averaged: Vec<Option<[f64; 3]>> = sums
        .into_iter()
        .zip(counts)
        .map(|(sum, count)| (*count != 0).then(|| sum.map(|value| value / *count as f64)))
        .collect();
    let mut ranges = [[f64::INFINITY, f64::NEG_INFINITY]; 3];
    for rgb in averaged.iter().flatten() {
        for channel in 0..3 {
            ranges[channel][0] = ranges[channel][0].min(rgb[channel]);
            ranges[channel][1] = ranges[channel][1].max(rgb[channel]);
        }
    }
    let mut rgba = Vec::with_capacity(averaged.len() * 4);
    for rgb in averaged {
        let Some(rgb) = rgb else {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        };
        for channel in 0..3 {
            let [min, max] = ranges[channel];
            let normalized = if min.is_finite() && max > min { (rgb[channel] - min) / (max - min) } else { 0.0 };
            rgba.push((normalized.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    rgba
}

fn finish_premultiplied_average(sums: [u64; 4], count: u64) -> [u8; 4] {
    let alpha_sum = sums[3];
    let rgb = |sum: u64| sum.checked_div(alpha_sum).unwrap_or(0).min(255) as u8;
    [rgb(sums[0]), rgb(sums[1]), rgb(sums[2]), (alpha_sum / count.max(1)).min(255) as u8]
}
