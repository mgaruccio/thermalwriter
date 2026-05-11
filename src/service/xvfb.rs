//! Xvfb process manager: spawns/owns Xvfb and child application processes.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::os::unix::process::CommandExt;
use anyhow::{Context, Result, bail};
use log::info;

/// Handle to a running Xvfb instance and its child application.
/// Dropping this handle kills both process groups and cleans up the temp directory.
pub struct XvfbHandle {
    xvfb_process: Child,
    child_process: Option<Child>,
    display_num: u32,
    fbdir: PathBuf,
    screen_file: PathBuf,
}

impl XvfbHandle {
    /// Path to the XWD screen file (for XvfbSource to mmap).
    pub fn screen_file(&self) -> &Path {
        &self.screen_file
    }

    /// The display number (e.g., 99 for `:99`).
    pub fn display_num(&self) -> u32 {
        self.display_num
    }
}

/// Send SIGTERM to a process group, then SIGKILL after a short wait if any
/// processes remain. Uses libc::killpg so the entire child process tree is
/// reaped, not just the direct child shell.
fn kill_process_group(pid: u32) {
    let pgid = pid as i32;
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    // Give the group up to 300 ms to exit cleanly before SIGKILL.
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        // If killpg returns ESRCH the group is gone — done.
        let rc = unsafe { libc::killpg(pgid, 0) };
        if rc != 0 || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

impl Drop for XvfbHandle {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child_process {
            let pid = child.id();
            kill_process_group(pid);
            let _ = child.wait();
            info!("Killed child process group (pgid {})", pid);
        }
        let xvfb_pid = self.xvfb_process.id();
        kill_process_group(xvfb_pid);
        let _ = self.xvfb_process.wait();
        info!("Killed Xvfb process group (pgid {}, display :{})", xvfb_pid, self.display_num);
        // Clean up temp fbdir
        let _ = std::fs::remove_dir_all(&self.fbdir);
    }
}

/// Find an unused X display number by checking for lock files.
fn find_unused_display() -> Result<u32> {
    for n in 99..200 {
        let lock_file = format!("/tmp/.X{}-lock", n);
        if !Path::new(&lock_file).exists() {
            return Ok(n);
        }
    }
    bail!("No available X display number found (checked :99 through :199)")
}

/// Start Xvfb and a child application, returning a handle that owns both processes.
///
/// `command` is executed via `sh -c` inside the virtual display (e.g., "conky -c foo.conf").
/// `width` and `height` set the virtual screen dimensions.
pub fn start(command: &str, width: u32, height: u32) -> Result<XvfbHandle> {
    let display_num = find_unused_display()?;
    let display = format!(":{}", display_num);

    // Create temp directory for fbdir
    let fbdir = std::env::temp_dir().join(format!("thermalwriter-xvfb-{}", display_num));
    std::fs::create_dir_all(&fbdir)
        .with_context(|| format!("Failed to create fbdir: {}", fbdir.display()))?;

    let screen_spec = format!("{}x{}x24", width, height);

    // Spawn Xvfb in its own process group so Drop can killpg the whole tree.
    let xvfb_process = Command::new("Xvfb")
        .arg(&display)
        .args(["-screen", "0", &screen_spec])
        .args(["-fbdir", &fbdir.to_string_lossy()])
        .args(["-ac", "-nolisten", "tcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .context("Failed to spawn Xvfb — is xvfb installed?")?;

    info!("Spawned Xvfb on display {} (pid {})", display, xvfb_process.id());

    // Wait for screen file to appear
    let screen_file = fbdir.join("Xvfb_screen0");

    // Build handle now (child_process: None) so Drop fires correctly on any failure below.
    let mut handle = XvfbHandle {
        xvfb_process,
        child_process: None,
        display_num,
        fbdir: fbdir.clone(),
        screen_file: screen_file.clone(),
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !screen_file.exists() {
        if Instant::now() > deadline {
            // Dropping handle kills Xvfb and cleans fbdir.
            bail!("Xvfb screen file did not appear within 5 seconds: {}", screen_file.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    info!("Xvfb screen file ready: {}", screen_file.display());

    // Spawn the child application in its own process group with DISPLAY set.
    // process_group(0) makes child.id() == child's pgid, so killpg(child.id())
    // kills the entire subtree (sh + any grandchildren it spawns).
    let child_process = Command::new("sh")
        .args(["-c", command])
        .env("DISPLAY", &display)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Failed to spawn child command: {}", command))?;

    info!("Spawned child application: {} (pid {})", command, child_process.id());
    handle.child_process = Some(child_process);

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_unused_display_returns_valid_number() {
        let num = find_unused_display().unwrap();
        assert!(num >= 99 && num < 200);
        // Verify the lock file doesn't exist
        let lock = format!("/tmp/.X{}-lock", num);
        assert!(!Path::new(&lock).exists());
    }

    /// Dropping XvfbHandle must kill the entire child process group, not just the
    /// direct sh child. Uses a unique sleep duration (sleep 9473) as a sentinel —
    /// if the grandchild survives Drop, pgrep finds it and the test fails.
    ///
    /// With the old child.kill() implementation this test FAILS because the
    /// backgrounded sleep outlives its sh parent. With killpg it passes.
    #[test]
    fn drop_kills_entire_process_group_not_just_direct_child() {
        // Use a unique sleep duration as sentinel so pgrep is unambiguous.
        let handle = start("sleep 9473 &", 480, 480)
            .expect("start() requires Xvfb — skipping if not available");

        // Give the grandchild a moment to be scheduled.
        std::thread::sleep(Duration::from_millis(200));

        // Drop the handle — this should kill the entire process group.
        drop(handle);

        // Give the OS a moment to reap processes.
        std::thread::sleep(Duration::from_millis(300));

        // Assert no process with our sentinel sleep duration remains.
        let output = Command::new("pgrep")
            .args(["-f", "sleep 9473"])
            .output()
            .expect("pgrep must be available");

        assert!(
            !output.status.success(),
            "sleep 9473 grandchild process is still alive after XvfbHandle drop — \
             process group was not killed (pgrep output: {})",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
