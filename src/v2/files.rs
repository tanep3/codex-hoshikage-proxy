//! File descriptor based traversal: no symlinks, mount traversal, special files or hard links.
use super::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
pub fn open_source(root: &Path, relative: &str) -> Result<File> {
    let m = std::fs::symlink_metadata(root)?;
    open_source_at(root, relative, (m.dev(), m.ino()))
}
pub fn open_source_at(root: &Path, relative: &str, identity: (u64, u64)) -> Result<File> {
    if relative.is_empty()
        || relative.contains('\\')
        || relative
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(Error::code(400, "invalid_argument"));
    }
    let relative = CString::new(relative).map_err(|_| Error::code(400, "invalid_argument"))?;
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    let actual = dir.metadata()?;
    if (actual.dev(), actual.ino()) != identity {
        return Err(Error::code(409, "workspace_identity_mismatch"));
    }
    // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV, fail closed on unsupported kernels.
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x01,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir.as_raw_fd(),
            relative.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOENT) => Error::code(404, "source_not_found"),
            _ => Error::code(403, "source_access_denied"),
        });
    }
    let file = unsafe { File::from_raw_fd(fd as i32) };
    let m = file.metadata()?;
    if !m.is_file() || m.nlink() != 1 {
        return Err(Error::code(403, "source_access_denied"));
    }
    Ok(file)
}
pub fn copy(source: &mut File, dest: &Path, limit: u64) -> Result<(u64, String)> {
    copy_with_timeout(source, dest, limit, 120)
}
pub fn copy_with_timeout(
    source: &mut File,
    dest: &Path,
    limit: u64,
    seconds: u64,
) -> Result<(u64, String)> {
    let started = std::time::Instant::now();
    let before = source.metadata()?;
    if before.len() > limit {
        return Err(Error::code(413, "artifact_too_large"));
    }
    let mut target = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(dest)?;
    let result = (|| {
        let mut hash = Sha256::new();
        let mut total = 0;
        let mut buffer = [0u8; 65536];
        loop {
            if started.elapsed() > std::time::Duration::from_secs(seconds) {
                return Err(Error::code(409, "capture_timeout"));
            }
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > limit {
                return Err(Error::code(413, "artifact_too_large"));
            }
            target.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
        }
        let after = source.metadata()?;
        if before.len() != after.len()
            || after.len() != total
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(Error::code(409, "source_changed"));
        }
        target.sync_all()?;
        Ok((total, format!("{:x}", hash.finalize())))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}
pub fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub fn publish(
    staging: &Path,
    blobs: &Path,
    key: &str,
    metadata: &serde_json::Value,
) -> Result<()> {
    let manifest = blobs.join(format!("{key}.manifest"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&manifest)?;
    file.write_all(serde_json::to_string(metadata)?.as_bytes())?;
    file.sync_all()?;
    sync_directory(blobs)?;
    std::fs::rename(staging, blobs.join(key))?;
    sync_directory(blobs)
}
pub fn verify(path: &Path, size: u64, expected: &str) -> Result<()> {
    let mut f = File::open(path)?;
    let mut digest = Sha256::new();
    let mut total = 0;
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err(Error::code(503, "content_corrupt"));
        }
        digest.update(&b[..n]);
    }
    if total != size || format!("{:x}", digest.finalize()) != expected {
        return Err(Error::code(503, "content_corrupt"));
    }
    Ok(())
}

pub fn require_free(path: &Path, reserve: u64, floor: u64) -> Result<()> {
    let c = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::code(400, "invalid_argument"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Error::code(503, "store_unavailable"));
    }
    let stat = unsafe { stat.assume_init() };
    let free = (stat.f_bavail as u128) * (stat.f_frsize as u128);
    if free < (reserve as u128) + (floor as u128) {
        return Err(Error::code(507, "storage_capacity_exceeded"));
    }
    Ok(())
}
