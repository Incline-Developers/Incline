/// A user-selected source independent of native filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceRef {
    pub(crate) name: String,
    pub(crate) byte_len: usize,
}

/// File contents passed to byte-oriented importers on both native and web.
#[derive(Clone, Debug)]
pub(crate) struct InputFile {
    pub(crate) source: SourceRef,
    pub(crate) bytes: Vec<u8>,
    pub(crate) reservation: Option<crate::app::memory::MemoryReservation>,
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn read_browser_handle(handle: rfd::FileHandle) -> Result<InputFile, String> {
    let file = handle.inner();
    let size = file.size();
    if !size.is_finite() || size < 0.0 || size > crate::app::memory::INPUT_FILE_LIMIT as f64 {
        return Err(format!("{} is too large; browser imports are limited to 2 GiB", file.name()));
    }
    let len = size as usize;
    let reservation = crate::app::memory::reserve(len, &format!("input '{}'", file.name()))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|error| format!("could not allocate {len} bytes for {}: {error}", file.name()))?;

    let mut start = 0usize;
    while start < len {
        let end = start.saturating_add(crate::app::memory::FILE_CHUNK_BYTES).min(len);
        let blob = file
            .slice_with_f64_and_f64(start as f64, end as f64)
            .map_err(|error| format!("could not slice {}: {error:?}", file.name()))?;
        let value = wasm_bindgen_futures::JsFuture::from(blob.array_buffer())
            .await
            .map_err(|error| format!("could not read {}: {error:?}", file.name()))?;
        let chunk = js_sys::Uint8Array::new(&value);
        let old_len = bytes.len();
        bytes.resize(old_len + chunk.length() as usize, 0);
        chunk.copy_to(&mut bytes[old_len..]);
        start = end;
    }
    Ok(InputFile {
        source: SourceRef { name: file.name(), byte_len: len },
        bytes,
        reservation: Some(reservation),
    })
}
