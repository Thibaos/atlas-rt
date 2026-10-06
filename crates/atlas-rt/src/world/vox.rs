use anyhow::{Context, anyhow, bail};

/// # Errors
///
/// Returns an error if the file loading failed, the palette is too large, or
/// the file contains an unsupported `IMAP` map.
pub fn open_bytes(bytes: &[u8]) -> anyhow::Result<dot_vox::DotVoxData> {
    let data = dot_vox::load_bytes(bytes).map_err(|e| anyhow!(e))?;

    if data.palette.len() > 256 {
        bail!("palette length is greater than 256");
    }

    validate_imap(bytes)?;

    Ok(data)
}

struct ChunkSlice {
    kind: [u8; 4],
    content_start: usize,
    content_end: usize,
    children_start: usize,
    end: usize,
}

fn read_u32(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    let end = offset
        .checked_add(4)
        .context("chunk size offset overflowed")?;
    let slice = bytes
        .get(offset..end)
        .with_context(|| format!("chunk header ended at {end}"))?;
    let value: [u8; 4] = slice
        .try_into()
        .map_err(|_| anyhow!("chunk size was not four bytes"))?;

    Ok(u32::from_le_bytes(value))
}

fn chunk_at(bytes: &[u8], offset: usize) -> anyhow::Result<Option<ChunkSlice>> {
    if offset == bytes.len() {
        return Ok(None);
    }

    if offset > bytes.len() {
        bail!("chunk offset {offset} is past the file");
    }

    let kind_end = offset
        .checked_add(4)
        .context("chunk id offset overflowed")?;
    let content_size_offset = offset
        .checked_add(4)
        .context("chunk content size offset overflowed")?;
    let children_size_offset = offset
        .checked_add(8)
        .context("chunk child size offset overflowed")?;
    let header_end = offset
        .checked_add(12)
        .context("chunk header offset overflowed")?;

    if header_end > bytes.len() {
        bail!("chunk header ended at {header_end}");
    }

    let kind: [u8; 4] = bytes
        .get(offset..kind_end)
        .context("chunk id was missing")?
        .try_into()
        .map_err(|_| anyhow!("chunk id was not four bytes"))?;
    let content_size = usize::try_from(read_u32(bytes, content_size_offset)?)
        .context("chunk content size does not fit usize")?;
    let children_size = usize::try_from(read_u32(bytes, children_size_offset)?)
        .context("chunk child size does not fit usize")?;
    let content_start = header_end;
    let content_end = content_start
        .checked_add(content_size)
        .context("chunk content end overflowed")?;
    let children_start = content_end;
    let end = children_start
        .checked_add(children_size)
        .context("chunk end overflowed")?;

    if end > bytes.len() {
        bail!("chunk ended at {end}, past the file");
    }

    Ok(Some(ChunkSlice {
        kind,
        content_start,
        content_end,
        children_start,
        end,
    }))
}

fn chunk_kind(bytes: &[u8], offset: usize) -> Option<[u8; 4]> {
    let end = offset.checked_add(4)?;
    let kind = bytes.get(offset..end)?;

    kind.try_into().ok()
}

fn validate_imap_chunk(bytes: &[u8], chunk: &ChunkSlice) -> anyhow::Result<()> {
    if chunk.children_start != chunk.end {
        bail!("IMAP must not contain child chunks");
    }

    let map_length = chunk
        .content_end
        .checked_sub(chunk.content_start)
        .context("IMAP content bounds were invalid")?;

    if map_length != dot_vox::DEFAULT_INDEX_MAP.len() {
        bail!("IMAP length {map_length} is not 256");
    }

    let map = bytes
        .get(chunk.content_start..chunk.content_end)
        .context("IMAP content was missing")?;

    if map != dot_vox::DEFAULT_INDEX_MAP {
        bail!("non-default IMAP is unsupported");
    }

    Ok(())
}

fn validate_child_chunks(bytes: &[u8], start: usize, end: usize) -> anyhow::Result<()> {
    let mut offset = start;

    while offset < end {
        let chunk = match chunk_at(bytes, offset) {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(()),
            Err(error) => {
                if chunk_kind(bytes, offset) == Some(*b"IMAP") {
                    return Err(error);
                }

                return Ok(());
            }
        };

        if &chunk.kind == b"IMAP" {
            validate_imap_chunk(bytes, &chunk)?;
        }

        if chunk.children_start < chunk.end {
            validate_child_chunks(bytes, chunk.children_start, chunk.end)?;
        }

        offset = chunk.end;
    }

    if offset != end {
        bail!("child chunks ended at {offset}, expected {end}");
    }

    Ok(())
}

fn validate_imap(bytes: &[u8]) -> anyhow::Result<()> {
    let main = chunk_at(bytes, 8)?.ok_or_else(|| anyhow!("VOX file has no MAIN chunk"))?;

    if &main.kind != b"MAIN" {
        bail!("VOX file does not start with MAIN");
    }

    if main.end != bytes.len() {
        bail!("VOX file has data after MAIN");
    }

    validate_child_chunks(bytes, main.children_start, main.end)
}

#[cfg(test)]
mod tests {
    use super::open_bytes;

    use crate::world::test_support::expect_error;

    fn chunk(kind: [u8; 4], content: &[u8], children: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&kind);
        bytes.extend_from_slice(
            &u32::try_from(content.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(children.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        bytes.extend_from_slice(content);
        bytes.extend_from_slice(children);
        bytes
    }

    fn vox_with_imap(map: Option<&[u8]>) -> Vec<u8> {
        vox_with_imap_children(map, &[])
    }

    fn vox_with_imap_children(map: Option<&[u8]>, imap_children: &[u8]) -> Vec<u8> {
        let size = chunk(*b"SIZE", &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[]);
        let voxel = chunk(*b"XYZI", &[1, 0, 0, 0, 0, 0, 0, 2], &[]);
        let palette = chunk(*b"RGBA", &[0; 1024], &[]);
        let mut children = [size, voxel, palette].concat();

        if let Some(map) = map {
            children.extend_from_slice(&chunk(*b"IMAP", map, imap_children));
        }

        let main = chunk(*b"MAIN", &[], &children);

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);
        bytes
    }

    #[test]
    fn default_imap_is_accepted_as_a_no_op() {
        let bytes = vox_with_imap(Some(dot_vox::DEFAULT_INDEX_MAP));

        assert!(open_bytes(&bytes).is_ok());
    }

    #[test]
    fn non_default_imap_is_rejected() {
        let mut map = dot_vox::DEFAULT_INDEX_MAP.to_vec();

        if let Some(first) = map.first_mut() {
            *first = 2;
        }

        let bytes = vox_with_imap(Some(&map));
        let error = expect_error(open_bytes(&bytes), "non-default IMAP must reject");

        assert!(error.to_string().contains("non-default IMAP"));
    }

    #[test]
    fn malformed_imap_is_rejected() {
        let bytes = vox_with_imap(Some(&[1, 2]));
        let error = expect_error(open_bytes(&bytes), "malformed IMAP must reject");

        assert!(error.to_string().contains("IMAP length"));
    }

    fn vox_with_nested_imap(map: &[u8]) -> Vec<u8> {
        let size = chunk(*b"SIZE", &[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[]);
        let voxel = chunk(*b"XYZI", &[1, 0, 0, 0, 0, 0, 0, 2], &[]);
        let palette = chunk(*b"RGBA", &[0; 1024], &[]);
        let nested = chunk(*b"NOPE", &[], &chunk(*b"IMAP", map, &[]));
        let children = [size, voxel, palette, nested].concat();
        let main = chunk(*b"MAIN", &[], &children);

        let mut bytes = b"VOX ".to_vec();
        bytes.extend_from_slice(&150u32.to_le_bytes());
        bytes.extend_from_slice(&main);
        bytes
    }

    #[test]
    fn imap_with_child_chunks_is_rejected() {
        let bytes =
            vox_with_imap_children(Some(dot_vox::DEFAULT_INDEX_MAP), &chunk(*b"NOPE", &[], &[]));
        let error = expect_error(open_bytes(&bytes), "IMAP child chunks must reject");

        assert!(
            error
                .to_string()
                .contains("IMAP must not contain child chunks")
        );
    }

    #[test]
    fn nested_non_default_imap_is_rejected() {
        let mut map = dot_vox::DEFAULT_INDEX_MAP.to_vec();

        if let Some(first) = map.first_mut() {
            *first = 2;
        }
        let bytes = vox_with_nested_imap(&map);
        let error = expect_error(open_bytes(&bytes), "nested non-default IMAP must reject");

        assert!(error.to_string().contains("non-default IMAP"));
    }

    #[test]
    fn nested_default_imap_is_accepted_as_a_no_op() {
        let bytes = vox_with_nested_imap(dot_vox::DEFAULT_INDEX_MAP);

        assert!(open_bytes(&bytes).is_ok());
    }

    #[test]
    fn data_after_main_is_rejected() {
        let mut bytes = vox_with_imap(None);
        bytes.extend_from_slice(b"trailing");
        let error = expect_error(open_bytes(&bytes), "trailing data must reject");

        assert!(error.to_string().contains("data after MAIN"));
    }
}
