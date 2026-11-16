//! memfd, format table and dma-buf sync file syscalls: the unsafe code outside `vulkan`.

use std::ffi::c_void;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::{self, NonNull};

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::ioctl;
use rustix::mm::{MapFlags, ProtFlags};

pub(crate) struct ShmMemory {
    fd: OwnedFd,
    ptr: NonNull<c_void>,
    len: usize,
}

// SAFETY: the mapping is owned by this value and only reached through `&self` or `&mut self`.
unsafe impl Send for ShmMemory {}

impl ShmMemory {
    pub fn new(len: usize) -> rustix::io::Result<Self> {
        let fd = rustix::fs::memfd_create(
            "deskshell-capture",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        let _ = rustix::fs::fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW);
        // SAFETY: a fresh shared mapping of a memfd we just sized to `len`. Nothing else in
        // this process maps it.
        let raw = unsafe {
            rustix::mm::mmap(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let ptr = NonNull::new(raw).ok_or(rustix::io::Errno::NOMEM)?;
        Ok(Self { fd, ptr, len })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// The compositor writes only between `capture` and `ready`, and the worker reads only
    /// after `ready`, so the bytes are stable while borrowed.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` maps `len` bytes for the life of `self`. See the doc comment for why
        // no write races the read.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().cast::<u8>(), self.len) }
    }
}

impl Drop for ShmMemory {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` came from `mmap` in `new` and are unmapped only here.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr(), self.len) };
    }
}

pub(crate) fn read_table(fd: BorrowedFd<'_>, len: usize) -> rustix::io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: a private read-only mapping of `len` bytes, as the protocol requires. It is
    // copied and unmapped before returning.
    unsafe {
        let raw = rustix::mm::mmap(
            ptr::null_mut(),
            len,
            ProtFlags::READ,
            MapFlags::PRIVATE,
            fd,
            0,
        )?;
        let bytes = std::slice::from_raw_parts(raw.cast::<u8>(), len).to_vec();
        let _ = rustix::mm::munmap(raw, len);
        Ok(bytes)
    }
}

#[repr(C)]
struct ExportSyncFile {
    flags: u32,
    fd: i32,
}

const DMA_BUF_BASE: u8 = b'b';
const DMA_BUF_SYNC_READ: u32 = 1;

pub(crate) fn export_sync_file(dmabuf: BorrowedFd<'_>) -> rustix::io::Result<OwnedFd> {
    let mut arg = ExportSyncFile { flags: DMA_BUF_SYNC_READ, fd: -1 };
    // SAFETY: DMA_BUF_IOCTL_EXPORT_SYNC_FILE is _IOWR('b', 2, struct dma_buf_export_sync_file)
    // and `ExportSyncFile` has that struct's layout.
    unsafe {
        let control = ioctl::Updater::<
            { ioctl::opcode::read_write::<ExportSyncFile>(DMA_BUF_BASE, 2) },
            _,
        >::new(&mut arg);
        ioctl::ioctl(dmabuf, control)?;
    }
    if arg.fd < 0 {
        return Err(rustix::io::Errno::INVAL);
    }
    // SAFETY: on success the kernel stored a new file descriptor that we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(arg.fd) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_memory_is_zeroed_and_sized() {
        let memory = ShmMemory::new(4096).unwrap();
        assert_eq!(memory.bytes().len(), 4096);
        assert!(
            memory
                .bytes()
                .iter()
                .all(|&byte| byte == 0)
        );
    }

    #[test]
    fn reads_a_table_from_a_memfd() {
        let fd = rustix::fs::memfd_create("table", MemfdFlags::CLOEXEC).unwrap();
        rustix::io::write(
            &fd,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        )
        .unwrap();
        assert_eq!(read_table(fd.as_fd(), 16).unwrap()[15], 16);
        assert!(
            read_table(fd.as_fd(), 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sync_file_export_fails_cleanly_on_a_memfd() {
        let fd = rustix::fs::memfd_create("not-a-dmabuf", MemfdFlags::CLOEXEC).unwrap();
        assert!(export_sync_file(fd.as_fd()).is_err());
    }
}
