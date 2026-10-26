//! A render image pulled back to the host as a PNG.
//!
//! The renderer publishes linear radiance in an `R16G16B16A16_SFLOAT` image and
//! no display encoding exists on the engine side (ADR 0006), so a saved frame
//! goes through the ACES fit and gamma the host's composite shader applies.
//! Raw radiance in a PNG reads as a near-black picture, and the fit is what
//! both the Godot example and a viewer of the standalone capture expect.

use std::{fs::File, io::BufWriter, path::Path};

use anyhow::Context;
use half::f16;
use vulkano::{
    buffer::{BufferCreateInfo, BufferUsage, Subbuffer},
    image::Image,
    memory::allocator::{AllocationCreateInfo, DeviceLayout, MemoryTypeFilter},
};
use vulkano_taskgraph::{
    Id,
    command_buffer::CopyImageToBufferInfo,
    resource::{AccessTypes, HostAccessType, ImageLayoutType},
};

use crate::render::{context::RenderContext, pipeline::task::RenderMode};

/// The host-side copy of one frame, in the bytes a PNG stores: eight bits per
/// channel, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameCapture {
    pub extent: [u32; 2],
    pub rgba: Vec<u8>,
}

/// The ACES fit the composite shader uses, per channel.
fn aces(channel: f32) -> f32 {
    let numerator = channel * 2.51f32.mul_add(channel, 0.03);
    let denominator = channel.mul_add(2.43f32.mul_add(channel, 0.59), 0.14);

    (numerator / denominator).clamp(0.0, 1.0)
}

fn quantize(channel: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (channel.clamp(0.0, 1.0) * 255.0).round() as u8;

    byte
}

/// One pixel through the composite shader's color pipeline. Debug modes pass
/// the paint through ungraded, as the host draws them.
///
/// The shader's 8-bit dither is left out. It exists to hide the banding a
/// moving 8-bit store shows, and in a still image its one-level spread reads as
/// a moire over every flat surface, which is not the feature a capture is
/// taken to inspect.
fn encode(mode: RenderMode, linear: [f32; 3]) -> [u8; 4] {
    let mut color = linear;

    if mode == RenderMode::Voxel {
        let [r, g, b] = color.map(aces);
        color = [r.powf(1.0 / 2.2), g.powf(1.0 / 2.2), b.powf(1.0 / 2.2)];
    }

    let [r, g, b] = color;

    [quantize(r), quantize(g), quantize(b), 255]
}

fn half_to_f32(bits: u16) -> f32 {
    f16::from_bits(bits).to_f32()
}

/// The halves the copy wrote, as one `u16` per channel half.
fn halves(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect()
}

fn encode_pixels(texels: &[u16], mode: RenderMode) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(texels.len());

    for texel in texels.as_chunks::<4>().0 {
        let [red, green, blue, _alpha] = *texel;
        let linear = [half_to_f32(red), half_to_f32(green), half_to_f32(blue)];

        rgba.extend_from_slice(&encode(mode, linear));
    }

    rgba
}

/// Copies `image` back to host memory and encodes it for display.
///
/// The copy goes through the task graph rather than a plain command buffer,
/// because the graph is what knows the layout the ray pass left the image in
/// and records the barrier that keeps its contents.
///
/// # Errors
///
/// Returns an error if the frame is empty, the copy could not be submitted, or
/// the host buffer could not be read. The image must be the renderer's own: the
/// copy hands it to the host in `TRANSFER_SRC_OPTIMAL`.
pub fn read_frame(
    gpu: &RenderContext,
    image: Id<Image>,
    extent: [u32; 2],
    mode: RenderMode,
) -> anyhow::Result<FrameCapture> {
    let [width, height] = extent;

    if width == 0 || height == 0 {
        anyhow::bail!("a {width} by {height} frame has no pixels to save");
    }

    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(8))
        .context("the capture extent overflows a buffer size")?;

    gpu.resources.wait_idle()?;

    let buffer = gpu.resources.create_buffer(
        &BufferCreateInfo {
            usage: BufferUsage::TRANSFER_DST,
            ..BufferCreateInfo::default()
        },
        &AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_HOST
                | MemoryTypeFilter::HOST_RANDOM_ACCESS,
            ..AllocationCreateInfo::default()
        },
        DeviceLayout::new_unsized::<[u8]>(bytes).context("the capture buffer layout is invalid")?,
    )?;

    unsafe {
        vulkano_taskgraph::execute(
            &gpu.graphics_queue,
            &gpu.resources,
            gpu.graphics_flight_id,
            |cbf, _tcx| {
                cbf.copy_image_to_buffer(&CopyImageToBufferInfo {
                    src_image: image,
                    dst_buffer: buffer,
                    ..CopyImageToBufferInfo::default()
                });

                Ok(())
            },
            [(buffer, HostAccessType::Read)],
            [(buffer, AccessTypes::COPY_TRANSFER_WRITE)],
            [(
                image,
                AccessTypes::COPY_TRANSFER_READ,
                ImageLayoutType::Optimal,
            )],
        )
    }?;

    gpu.resources.wait_idle()?;

    let host: Subbuffer<[u8]> = Subbuffer::new(gpu.resources.buffer(buffer).buffer().clone());
    let halves = halves(&host.read()?);
    let rgba = encode_pixels(&halves, mode);

    let mut deletion = gpu.resources.create_deferred_batch();
    deletion.destroy_buffer(buffer);
    deletion.enqueue();

    Ok(FrameCapture { extent, rgba })
}

/// # Errors
///
/// Returns an error if the file cannot be created or the PNG cannot be
/// written.
pub fn save_png(path: &Path, capture: &FrameCapture) -> anyhow::Result<()> {
    let [width, height] = capture.extent;
    let file = File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);

    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header()?;

    writer.write_image_data(&capture.rgba)?;
    writer.finish()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{aces, encode, encode_pixels, quantize};
    use crate::render::pipeline::task::RenderMode;

    /// Radiance of 1.0 is what a lit surface reaches, so the fit has to keep it
    /// below white.
    #[test]
    fn aces_rolls_off_the_highlights() {
        assert!(aces(0.0).abs() < 1.0e-6);
        assert!((aces(1.0) - 0.8038).abs() < 0.0001, "{}", aces(1.0));
        assert!((aces(10.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn a_voxel_mode_pixel_takes_the_fit_and_the_gamma() {
        let encoded = encode(RenderMode::Voxel, [0.5; 3]);
        let expected = quantize(aces(0.5).powf(1.0 / 2.2));

        for channel in encoded.iter().take(3) {
            assert_eq!(*channel, expected);
        }
    }

    #[test]
    fn debug_modes_store_the_paint_unencoded() {
        let encoded = encode(RenderMode::Normal, [1.0, 0.5, 0.25]);

        for (index, value) in [1.0, 0.5, 0.25].into_iter().enumerate() {
            let stored = encoded.get(index).copied().unwrap_or(0);

            assert_eq!(stored, quantize(value));
        }
    }

    #[test]
    fn every_texel_becomes_one_opaque_pixel() {
        let texels = [0x3C00u16; 24];
        let rgba = encode_pixels(&texels, RenderMode::Voxel);

        assert_eq!(rgba.len(), 3 * 2 * 4);

        for pixel in rgba.as_chunks::<4>().0 {
            assert_eq!(pixel.last(), Some(&255));
        }
    }
}
