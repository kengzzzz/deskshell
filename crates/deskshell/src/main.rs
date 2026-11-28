//! The deskshell binary.

fn main() -> std::process::ExitCode {
    // A fixed threshold stops glibc from raising it after a frame-sized buffer is freed and
    // then keeping later frames in its arenas.
    // SAFETY: mallopt only changes malloc settings and is called before other threads exist.
    unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024) };
    deskshell::main()
}
