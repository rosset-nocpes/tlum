//! A file mapped read-only into memory: the dictionaries' indexes, which a lookup binary-searches.
//! Mapped, only the pages a search touches are read, and they are the kernel's page cache, which
//! it can take back, rather than tens of megabytes of heap the daemon holds for its whole life.

use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Mapped {
    ptr: *const u8,
    len: usize,
    /// The file mapped, and what it was when it was.
    source: Option<(std::fs::File, PathBuf, Stamp)>,
}

/// A file's inode, size and modification time.
type Stamp = (u64, u64, i64, i64);

fn stamp(m: &std::fs::Metadata) -> Stamp { (m.ino(), m.len(), m.mtime(), m.mtime_nsec()) }

// SAFETY: the mapping is read-only and owned by this value; any thread may read it.
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    /// Maps `path`. tlum replaces a dictionary by renaming a new folder over it, which leaves a
    /// mapped file as it was; a file rewritten in place (a `cp` over the folder) shows in
    /// `changed`, and its reader is opened again before it's read (`library::open`). Reading a
    /// page past the end of a file cut short while it's mapped would kill the process, so only
    /// a copy made during a lookup is a risk.
    pub fn open(path: &Path) -> Result<Mapped, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?;
        let len = meta.len() as usize;
        let source = Some((file, path.to_path_buf(), stamp(&meta)));
        if len == 0 { return Ok(Mapped { source, ..Mapped::empty() }); }
        let fd = source.as_ref().unwrap().0.as_raw_fd();
        // SAFETY: a fresh read-only private mapping of a file we opened; checked for failure.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) };
        if ptr == libc::MAP_FAILED { return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error())); }
        Ok(Mapped { ptr: ptr.cast(), len, source })
    }

    /// No bytes, for a file that isn't there.
    pub fn empty() -> Mapped { Mapped { ptr: std::ptr::NonNull::dangling().as_ptr(), len: 0, source: None } }

    /// Whether the file was written to since it was mapped, or another took its name.
    pub fn changed(&self) -> bool {
        let Some((file, path, was)) = &self.source else { return false };
        let now = file.metadata().map(|m| stamp(&m)).ok();
        now.as_ref() != Some(was) || std::fs::metadata(path).map(|m| m.ino()).ok() != Some(was.0)
    }

    /// Lets the kernel take back the pages read so far (opening reads the whole index once to
    /// find its entries); a later read brings in just what it touches.
    pub fn release(&self) {
        if self.len == 0 { return; }
        // SAFETY: advice on our own mapping; a clean read-only page reads back the same.
        unsafe { libc::madvise(self.ptr as *mut libc::c_void, self.len, libc::MADV_DONTNEED) };
    }
}

impl std::ops::Deref for Mapped {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `len` readable bytes at `ptr` for as long as the mapping lives (or none).
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: unmaps the mapping `open` made, once.
        if self.len > 0 { unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) }; }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn maps_a_file_and_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("tlum-mapped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), b"hello").unwrap();
        std::fs::write(dir.join("b"), b"").unwrap();
        let a = super::Mapped::open(&dir.join("a")).unwrap();
        a.release();
        assert_eq!(&a[..], b"hello", "reads back after its pages are released");
        assert!(super::Mapped::open(&dir.join("b")).unwrap().is_empty());
        assert!(super::Mapped::open(&dir.join("missing")).is_err());
        assert!(!a.changed());
        std::fs::write(dir.join("a"), b"hello, again").unwrap();
        assert!(a.changed(), "rewritten in place");
        let a = super::Mapped::open(&dir.join("a")).unwrap();
        std::fs::write(dir.join("c"), b"new").unwrap();
        std::fs::rename(dir.join("c"), dir.join("a")).unwrap();
        assert!(a.changed(), "replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
