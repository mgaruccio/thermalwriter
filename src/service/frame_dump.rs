// Frame dump: writes the last-rendered xvfb JPEG frame to a tmpfs path for GUI preview.
//
// Path contract (shared with GUI read_frame command):
//   $XDG_RUNTIME_DIR/thermalwriter/last.jpg
// No /tmp fallback: streamed frames can expose private window contents, so the
// daemon fails closed if no per-user runtime directory is available.
//
// Writes are atomic (fsync + temp file + rename) so the GUI never reads a
// partial JPEG.  Only the tick loop calls write_frame_atomic in production
// (single writer), so a single fixed temp name is correct and sufficient.

use anyhow::{Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Return the private directory where the last-frame JPEG is written.
///
/// Requires `$XDG_RUNTIME_DIR`; falling back to `/tmp` would expose streamed
/// window contents through a shared namespace.
pub fn frame_dir() -> Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set; refusing to dump stream frames to shared /tmp")?;
    Ok(PathBuf::from(runtime).join("thermalwriter"))
}

/// The canonical path of the last-frame JPEG.
pub fn frame_path(dir: &Path) -> PathBuf {
    dir.join("last.jpg")
}

/// Atomically write `jpeg_bytes` to `dir/last.jpg`.
///
/// Creates `dir` if it does not exist.  Writes to a fixed sibling temp file
/// (`last.jpg.tmp`), fsyncs to flush kernel buffers, then renames atomically.
/// Only the tick loop calls this in production (single writer), so a single
/// fixed temp name is correct — no suffix needed.
///
/// Readers always see either the previous complete frame or the new one;
/// they never observe a partial write.  This matches the fsync+rename pattern
/// used by `config.rs` save_* helpers.
pub fn write_frame_atomic(dir: &Path, jpeg_bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let dest = frame_path(dir);
    let tmp = dir.join("last.jpg.tmp");

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(jpeg_bytes)?;
    file.sync_all()?;
    drop(file);

    std::fs::rename(&tmp, &dest)?;

    Ok(())
}

/// Remove `dir/last.jpg` and the sibling temp file if present.
///
/// Called when the active mode transitions away from xvfb so no stale frame
/// remains on tmpfs.  Errors are ignored — the files simply may not exist.
pub fn clear_frame(dir: &Path) {
    let _ = std::fs::remove_file(frame_path(dir));
    let _ = std::fs::remove_file(dir.join("last.jpg.tmp"));
}

/// Clear the published frame only when an installed source leaves streaming mode.
pub fn clear_frame_on_stream_exit(dir: &Path, was_streaming: bool, is_streaming: bool) {
    if was_streaming && !is_streaming {
        clear_frame(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // --- frame_dir ---

    // XDG_RUNTIME_DIR is a process-global env var; #[serial] prevents races
    // with any other test that reads or writes it in parallel.
    #[test]
    #[serial]
    fn frame_dir_uses_xdg_runtime_dir() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        let dir = frame_dir().unwrap();
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert_eq!(dir, PathBuf::from("/run/user/1000/thermalwriter"));
    }

    #[test]
    #[serial]
    fn frame_dir_rejects_missing_xdg_runtime_dir() {
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        let result = frame_dir();
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert!(
            result.is_err(),
            "frame_dir must fail closed without XDG_RUNTIME_DIR"
        );
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "frame dump directory must be private");
        }
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
    fn write_frame_atomic_reader_never_sees_partial_file() {
        // Single writer (production design), concurrent reader.  The reader
        // must always see either the previous complete payload or the new one —
        // never a partial write.  The rename(2) atomicity guarantee makes this
        // hold: the destination is swapped to the fully-written temp file.
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let dir = Arc::new(tmp.path().join("tw_test_c"));

        let payload_a: Vec<u8> = vec![0xAA; 4096];
        let payload_b: Vec<u8> = vec![0xBB; 4096];

        // Pre-seed so the reader's first read always finds a file.
        std::fs::create_dir_all(&*dir).unwrap();
        write_frame_atomic(&dir, &payload_a).unwrap();

        let barrier = Arc::new(Barrier::new(2)); // writer + reader

        let dir_w = Arc::clone(&dir);
        let pa = payload_a.clone();
        let pb = payload_b.clone();
        let bw = Arc::clone(&barrier);
        let writer = thread::spawn(move || {
            bw.wait();
            for i in 0..100u32 {
                let payload = if i % 2 == 0 { &pa } else { &pb };
                write_frame_atomic(&dir_w, payload).unwrap();
            }
        });

        let dir_r = Arc::clone(&dir);
        let pa2 = payload_a.clone();
        let pb2 = payload_b.clone();
        let br = Arc::clone(&barrier);
        let reader = thread::spawn(move || {
            br.wait();
            for _ in 0..200 {
                if let Ok(bytes) = std::fs::read(frame_path(&dir_r)) {
                    assert!(
                        bytes == pa2 || bytes == pb2,
                        "torn read: {} bytes (expected {} or {})",
                        bytes.len(),
                        pa2.len(),
                        pb2.len(),
                    );
                }
            }
        });

        writer.join().unwrap();
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
    fn clear_frame_also_removes_tmp_sibling() {
        // Verify that clear_frame removes last.jpg.tmp (the fixed temp name),
        // which may be left behind if the process was killed mid-write.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_f");
        std::fs::create_dir_all(&dir).unwrap();
        let tmp_path = dir.join("last.jpg.tmp");
        std::fs::write(&tmp_path, b"partial").unwrap();

        clear_frame(&dir);
        assert!(!tmp_path.exists());
    }

    #[test]
    fn clear_frame_is_idempotent_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tw_test_e_nonexistent");
        // Should not panic even if dir or file doesn't exist
        clear_frame(&dir);
    }

    #[test]
    fn stream_exit_clears_published_files_only_on_true_to_false_transition() {
        for (was_streaming, is_streaming, should_exist) in [
            (false, false, true),
            (false, true, true),
            (true, true, true),
            (true, false, false),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("transition");
            write_frame_atomic(&dir, b"published").unwrap();
            let tmp_path = dir.join("last.jpg.tmp");
            std::fs::write(&tmp_path, b"partial").unwrap();

            clear_frame_on_stream_exit(&dir, was_streaming, is_streaming);

            assert_eq!(
                frame_path(&dir).exists(),
                should_exist,
                "{was_streaming}->{is_streaming} last.jpg state"
            );
            assert_eq!(
                tmp_path.exists(),
                should_exist,
                "{was_streaming}->{is_streaming} temp state"
            );
        }
    }
}
