//! Xvfb process manager: spawns/owns Xvfb and child application processes.

use anyhow::{Context, Result, bail};
use log::info;
use std::io::Read as _;
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
        info!(
            "Killed Xvfb process group (pgid {}, display :{})",
            xvfb_pid, self.display_num
        );
        // Clean up temp fbdir
        let _ = std::fs::remove_dir_all(&self.fbdir);
    }
}

/// Start Xvfb and a child application, returning a handle that owns both processes.
///
/// Display allocation is atomic: Xvfb is invoked with `-displayfd` so it picks
/// and locks a free display number itself, then writes that number to the pipe.
/// This eliminates the TOCTOU race of the old lockfile-scan approach.
///
/// `command` is executed via `sh -c` inside the virtual display (e.g., "conky -c foo.conf").
/// `width` and `height` set the virtual screen dimensions.
pub fn start(command: &str, width: u32, height: u32) -> Result<XvfbHandle> {
    // Create a pipe: Xvfb writes the chosen display number to the write end,
    // we read it from the read end.
    let (pipe_read_fd, pipe_write_fd) = {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            bail!(
                "Failed to create pipe for -displayfd: {}",
                std::io::Error::last_os_error()
            );
        }
        (fds[0], fds[1])
    };

    // Use a temporary fbdir with a placeholder name; we rename it after
    // learning the display number from -displayfd. Use the pipe write-fd as a
    // unique suffix so concurrent calls in the same process don't collide.
    let tmp_fbdir = std::env::temp_dir().join(format!(
        "thermalwriter-xvfb-tmp-{}-{}",
        std::process::id(),
        pipe_write_fd
    ));
    std::fs::create_dir_all(&tmp_fbdir)
        .with_context(|| format!("Failed to create tmp fbdir: {}", tmp_fbdir.display()))?;

    let screen_spec = format!("{}x{}x24", width, height);

    // Spawn Xvfb. It inherits the write end of the pipe via -displayfd, writes
    // the chosen display number (as a decimal string + newline), then closes it.
    // We close the write end in this process after spawn so our read() sees EOF.
    let xvfb_process = unsafe {
        Command::new("Xvfb")
            .arg("-displayfd")
            .arg(pipe_write_fd.to_string())
            .args(["-screen", "0", &screen_spec])
            .args(["-fbdir", &tmp_fbdir.to_string_lossy()])
            .args(["-ac", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            // SAFETY: pre_exec runs in the child between fork and exec.
            // We must NOT close pipe_write_fd here — Xvfb needs it.
            // The read end is not inherited (it stays open in the parent only).
            .pre_exec(move || {
                // Close the read end in the child so it doesn't linger.
                libc::close(pipe_read_fd);
                Ok(())
            })
            .spawn()
            .context("Failed to spawn Xvfb — is Xvfb installed?")?
    };

    // Close the write end in the parent — once Xvfb writes and closes its copy,
    // our read will see EOF instead of hanging forever.
    unsafe { libc::close(pipe_write_fd) };

    // Read the display number from the pipe. Xvfb writes "<N>\n".
    // Wrap the fd in a File for safe buffered reading; the fd is owned here.
    let mut pipe_reader = unsafe { std::fs::File::from_raw_fd(pipe_read_fd) };
    let mut buf = String::new();
    pipe_reader
        .read_to_string(&mut buf)
        .context("Failed to read display number from Xvfb -displayfd pipe")?;
    drop(pipe_reader); // fd is now closed

    let display_num: u32 = buf
        .trim()
        .parse()
        .with_context(|| format!("Xvfb -displayfd returned non-numeric output: {:?}", buf))?;

    let display = format!(":{}", display_num);

    // Rename the fbdir to include the actual display number.
    let fbdir = std::env::temp_dir().join(format!("thermalwriter-xvfb-{}", display_num));
    // If a stale fbdir exists from a previous run, remove it first.
    let _ = std::fs::remove_dir_all(&fbdir);
    std::fs::rename(&tmp_fbdir, &fbdir).with_context(|| {
        format!(
            "Failed to rename fbdir {} → {}",
            tmp_fbdir.display(),
            fbdir.display()
        )
    })?;

    info!(
        "Spawned Xvfb on display {} (pid {})",
        display,
        xvfb_process.id()
    );

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
            bail!(
                "Xvfb screen file did not appear within 5 seconds: {}",
                screen_file.display()
            );
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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Failed to spawn child command: {}", command))?;

    info!(
        "Spawned child application: {} (pid {})",
        command,
        child_process.id()
    );
    handle.child_process = Some(child_process);

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Returns `true` if a process with the given PID exists (is alive).
    fn pid_alive(pid: u32) -> bool {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0
    }

    /// `-displayfd` must allocate a real display: the returned handle has a
    /// non-zero display_num, the Xvfb process is alive while the handle is
    /// held, the framebuffer screen file exists, and after Drop both the Xvfb
    /// process is dead and the fbdir has been removed.
    ///
    /// We do NOT assert on /tmp/.X{N}-lock because a real X server on the
    /// system may hold that file for lower-numbered displays, making the check
    /// unreliable.
    #[test]
    #[serial]
    fn displayfd_start_returns_valid_display() {
        let handle = start("true", 480, 480).expect("start must succeed");

        // display_num must be non-zero (Xvfb -displayfd allocates from 1+).
        assert!(
            handle.display_num() >= 1,
            "display_num {} is unexpectedly zero",
            handle.display_num()
        );

        // Xvfb must be alive (process exists).
        let xvfb_pid = handle.xvfb_process.id();
        assert!(
            pid_alive(xvfb_pid),
            "Xvfb process (pid {}) is not alive — Xvfb failed to start",
            xvfb_pid
        );

        // The screen file must exist (Xvfb initialised the framebuffer).
        assert!(
            handle.screen_file().exists(),
            "Xvfb screen file {} does not exist",
            handle.screen_file().display()
        );

        let fbdir = handle.fbdir.clone();
        drop(handle);

        // Give the OS a moment to reap Xvfb and clean up.
        std::thread::sleep(Duration::from_millis(400));

        // After Drop, Xvfb must be dead (its pid no longer alive).
        assert!(
            !pid_alive(xvfb_pid),
            "Xvfb process (pid {}) is still alive after XvfbHandle drop",
            xvfb_pid
        );

        // The fbdir must have been removed by Drop.
        assert!(
            !fbdir.exists(),
            "fbdir {} still exists after XvfbHandle drop",
            fbdir.display()
        );
    }

    /// Two sequential `start` calls must allocate distinct display numbers.
    /// Both Xvfb processes must be alive while their handles are held, and
    /// both handles must drop cleanly.
    #[test]
    #[serial]
    fn displayfd_two_sequential_starts_get_distinct_displays() {
        let h1 = start("true", 480, 480).expect("first start must succeed");
        let h2 = start("true", 480, 480).expect("second start must succeed");

        let n1 = h1.display_num();
        let n2 = h2.display_num();

        assert_ne!(
            n1, n2,
            "both start() calls returned display :{} — -displayfd must allocate unique displays",
            n1
        );

        // Both Xvfb processes must be alive while both handles are held.
        assert!(
            pid_alive(h1.xvfb_process.id()),
            "Xvfb process for display :{} is not alive",
            n1
        );
        assert!(
            pid_alive(h2.xvfb_process.id()),
            "Xvfb process for display :{} is not alive",
            n2
        );

        // Both screen files must exist.
        assert!(
            h1.screen_file().exists(),
            "screen file for display :{} not found",
            n1
        );
        assert!(
            h2.screen_file().exists(),
            "screen file for display :{} not found",
            n2
        );

        // Drop both — must not panic.
        drop(h1);
        drop(h2);
    }

    /// Dropping XvfbHandle must kill the entire child process group, not just the
    /// direct sh child. Uses a unique sleep duration (sleep 9473) as a sentinel —
    /// if the grandchild survives Drop, pgrep finds it and the test fails.
    ///
    /// With the old child.kill() implementation this test FAILS because the
    /// backgrounded sleep outlives its sh parent. With killpg it passes.
    #[test]
    #[serial]
    fn drop_kills_entire_process_group_not_just_direct_child() {
        // Use a unique sleep duration as sentinel so pgrep is unambiguous.
        let handle = match start("sleep 9473 &", 480, 480) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("skipping Xvfb process-group test: {err:#}");
                return;
            }
        };

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
