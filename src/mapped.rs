//! A file mapped read-only into memory: the dictionaries' indexes, which a lookup binary-searches.
//! Mapped, only the pages a search touches are read, and they are the kernel's page cache, which
//! it can take back, rather than tens of megabytes of heap the daemon holds for its whole life.

use std::os::fd::AsRawFd;
use std::path::Path;

pub struct Mapped { ptr: *const u8, len: usize }

// SAFETY: the mapping is read-only and owned by this value; any thread may read it.
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    /// Maps `path`. The files are never written in place (a dictionary is replaced by renaming a
    /// new folder over it), so the bytes stay as they were for as long as this lives.
    pub fn open(path: &Path) -> Result<Mapped, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?.len() as usize;
        if len == 0 { return Ok(Mapped::empty()); }
        // SAFETY: a fresh read-only private mapping of a file we opened; checked for failure.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED { return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error())); }
        Ok(Mapped { ptr: ptr.cast(), len })
    }

    /// No bytes, for a file that isn't there.
    pub fn empty() -> Mapped { Mapped { ptr: std::ptr::NonNull::dangling().as_ptr(), len: 0 } }

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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
