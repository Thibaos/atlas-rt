use anyhow::{anyhow, bail};

/// # Panics
///
/// Panics if `path` cannot be read or parsed as a `.vox` file.
#[must_use]
pub fn open_file(path: &str) -> dot_vox::DotVoxData {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("could not read {path}: {error}"));

    open_bytes(&bytes).unwrap_or_else(|error| panic!("could not load {path}: {error}"))
}

/// # Errors
///
/// Returns an error if the file loading failed or if the palette size is greater than 256
pub fn open_bytes(bytes: &[u8]) -> anyhow::Result<dot_vox::DotVoxData> {
    let data = dot_vox::load_bytes(bytes).map_err(|e| anyhow!(e))?;

    if data.palette.len() > 256 {
        bail!("palette length is greater than 256");
    }

    Ok(data)
}

#[must_use]
pub fn get_palette(data: &dot_vox::DotVoxData) -> [glam::Vec4; 256] {
    let mut array = [glam::Vec4::ZERO; 256];

    for (slot, color) in array.iter_mut().zip(data.palette.iter()) {
        *slot = glam::Vec4::new(
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            f32::from(color.a) / 255.0,
        );
    }

    array
}
