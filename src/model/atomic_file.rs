use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Write a sibling temporary created with `create_new`, then atomically replace
/// the destination. A unique sibling avoids following/truncating a predictable
/// temporary symlink and keeps the rename on the destination filesystem.
pub(crate) fn write_atomic(path: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let permissions = fs::metadata(path).ok().map(|metadata| metadata.permissions());
    let (tmp_path, mut file) = create_unique_sibling(path)?;

    let write_result = (|| -> Result<()> {
        write(&mut file)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions).with_context(|| format!("preserve permissions for {}", path.display()))?;
        }
        file.sync_all().with_context(|| format!("sync {}", tmp_path.display()))?;
        Ok(())
    })();
    drop(file);

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }

    if let Err(error) = replace_file(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }

    // Best-effort directory sync makes the rename durable on filesystems that
    // support syncing directory handles. It is deliberately non-fatal because
    // several supported platforms reject directory syncs.
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn create_unique_sibling(path: &Path) -> Result<(PathBuf, File)> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map_or_else(|| OsString::from("output"), OsString::from);
    for _ in 0..128 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = name.clone();
        temporary_name.push(format!(".tmp-{}-{id}", std::process::id()));
        let temporary_path = parent.join(temporary_name);
        match OpenOptions::new().write(true).create_new(true).open(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", temporary_path.display()));
            }
        }
    }
    anyhow::bail!("could not allocate a unique temporary beside {}", path.display())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both arguments are valid, NUL-terminated UTF-16 buffers for the
    // duration of the call; flags request same-volume atomic replacement.
    let succeeded = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
    if succeeded == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}
