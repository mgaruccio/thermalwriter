// Frame dump: writes the last-rendered xvfb JPEG frame to a tmpfs path for GUI preview.
//
// Path contract (shared with GUI Phase 3 Task 9 read_frame command):
//   $XDG_RUNTIME_DIR/thermalwriter/last.jpg
// Falls back to /tmp/thermalwriter/last.jpg if XDG_RUNTIME_DIR is unset.
//
// Writes are atomic (temp file + rename) so the GUI never reads a partial JPEG.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Return the directory where the last-frame JPEG is written.
///
/// Prefers `$XDG_RUNTIME_DIR/thermalwriter`; falls back to `/tmp/thermalwriter`
/// when the env var is unset.
pub fn frame_dir() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("thermalwriter")
    } else {
        PathBuf::from("/tmp/thermalwriter")
    }
}

/// The canonical path of the last-frame JPEG.
pub fn frame_path(dir: &Path) -> PathBuf {
    dir.join("last.jpg")
}

/// Atomically write `jpeg_bytes` to `dir/last.jpg`.
///
/// Creates `dir` if it does not exist. Writes to a per-caller temp file
/// (suffixed with `pid.thread_id`) then renames, so:
/// - The GUI always reads either the previous complete frame or the new
///   complete frame — never a partial write.
/// - Concurrent callers don't clobber each other's temp file before the
///   rename, even though in production only the single tick-loop thread
///   calls this.
pub fn write_frame_atomic(dir: &Path, jpeg_bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir)?;

    let dest = frame_path(dir);
    // Unique temp name per caller: avoids two concurrent writers racing on
    // the same .tmp path (one rename would truncate the other's write).
    let tid = {
        // std::thread::current().id() has no stable numeric representation;
        // use a pointer-cast of a stack variable as a cheap unique token.
        let x: u8 = 0;
        &x as *const u8 as usize
    };
    let tmp = dir.join(format!("last.jpg.{}.tmp", tid));

    std::fs::write(&tmp, jpeg_bytes)?;
    std::fs::rename(&tmp, &dest)?;

    Ok(())
}

/// Remove `dir/last.jpg` (and its temp sibling if present).
///
/// Called when the active mode transitions away from xvfb so no stale frame
/// remains on tmpfs.  Errors are ignored — the file simply may not exist.
pub fn clear_frame(dir: &Path) {
    let _ = std::fs::remove_file(frame_path(dir));
    let _ = std::fs::remove_file(dir.join("last.jpg.tmp"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    // --- frame_dir ---

    #[test]
    fn frame_dir_uses_xdg_runtime_dir() {
        // Temporarily set the env var; serial_test is NOT needed here because
        // we restore the original value before returning and the test does not
        // mutate any shared process state beyond the duration of this call.
        //
        // NOTE: env-var mutation in parallel tests is unsound in general.
        // These tests are in their own module and each uses a distinct key, so
        // they do not race with each other.  If the project adds more env-var
        // tests across threads, introduce #[serial] from the serial_test crate.
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        let dir = frame_dir();
        // restore
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert_eq!(dir, PathBuf::from("/run/user/1000/thermalwriter"));
    }

    // --- write_frame_atomic ---

    #[test]
    fn write_frame_atomic_creates_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_a");
        let data = b"FAKE_JPEG_BYTES";

        write_frame_atomic(&dir, data).unwrap();

        let out = std::fs::read(frame_path(&dir)).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn write_frame_atomic_exact_bytes_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_b");
        // Simulate a small real JPEG header prefix
        let jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00";

        write_frame_atomic(&dir, jpeg).unwrap();

        let out = std::fs::read(frame_path(&dir)).unwrap();
        assert_eq!(&out, jpeg);
    }

    #[test]
    fn write_frame_atomic_no_torn_file_under_concurrent_writes() {
        // Two threads hammer the same dir; reader must never see partial data.
        // We verify the invariant by checking that every read yields either
        // the "odd" payload or the "even" payload — never a mix or a zero-byte file.
        let tmp = tempfile::tempdir().unwrap();
        let dir = Arc::new(tmp.path().join("tw_test_c"));

        let odd_payload: Vec<u8> = vec![0xAA; 4096];
        let even_payload: Vec<u8> = vec![0xBB; 4096];

        let barrier = Arc::new(Barrier::new(3)); // writer1 + writer2 + reader

        let dir1 = Arc::clone(&dir);
        let odd = odd_payload.clone();
        let b1 = Arc::clone(&barrier);
        let w1 = thread::spawn(move || {
            b1.wait();
            for _ in 0..50 {
                write_frame_atomic(&dir1, &odd).unwrap();
            }
        });

        let dir2 = Arc::clone(&dir);
        let even = even_payload.clone();
        let b2 = Arc::clone(&barrier);
        let w2 = thread::spawn(move || {
            b2.wait();
            for _ in 0..50 {
                write_frame_atomic(&dir2, &even).unwrap();
            }
        });

        // Reader: starts after both writers are ready, checks each read.
        let dir3 = Arc::clone(&dir);
        let b3 = Arc::clone(&barrier);
        // Pre-seed so the first read doesn't fail on a missing file.
        std::fs::create_dir_all(&*dir).unwrap();
        write_frame_atomic(&dir, &odd_payload).unwrap();
        let reader = thread::spawn(move || {
            b3.wait();
            for _ in 0..100 {
                if let Ok(bytes) = std::fs::read(frame_path(&dir3)) {
                    // Must be exactly one of the two payloads — never partial.
                    assert!(
                        bytes == odd_payload || bytes == even_payload,
                        "torn read: {} bytes (expected {} or {})",
                        bytes.len(),
                        odd_payload.len(),
                        even_payload.len(),
                    );
                }
                // A missing file between writes is fine — clear_frame may run.
            }
        });

        w1.join().unwrap();
        w2.join().unwrap();
        reader.join().unwrap();
    }

    // --- clear_frame ---

    #[test]
    fn clear_frame_removes_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_d");
        write_frame_atomic(&dir, b"data").unwrap();

        assert!(frame_path(&dir).exists());
        clear_frame(&dir);
        assert!(!frame_path(&dir).exists());
    }

    #[test]
    fn clear_frame_is_idempotent_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_e_nonexistent");
        // Should not panic even if dir or file doesn't exist
        clear_frame(&dir);
    }
}
