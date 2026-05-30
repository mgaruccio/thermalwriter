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

/// Display numbers below this base are reserved for real desktop X servers
/// (Xorg, Xwayland). We start scanning from here to avoid collisions.
const DISPLAY_BASE: u32 = 100;
/// Maximum number of display candidates to try before giving up.
const DISPLAY_MAX_TRIES: u32 = 20;

/// Try to start Xvfb on a specific display number using `-displayfd`.
///
/// Returns `Ok(Some((process, display_num)))` if Xvfb bound that display,
/// `Ok(None)` if the display was already taken (Xvfb exited with empty pipe),
/// or `Err` if spawning itself failed.
fn try_start_xvfb_on(
    display_num: u32,
    fbdir: &Path,
    screen_spec: &str,
) -> Result<Option<(Child, u32)>> {
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

    // Pass an explicit display number as the starting candidate. When the
    // display is free Xvfb binds it and writes the number to the pipe. When
    // it is taken Xvfb exits with code 1 and writes nothing (empty pipe →
    // clean signal to try the next candidate).
    let xvfb_process = unsafe {
        Command::new("Xvfb")
            .arg(format!(":{}", display_num))
            .arg("-displayfd")
            .arg(pipe_write_fd.to_string())
            .args(["-screen", "0", screen_spec])
            .args(["-fbdir", &fbdir.to_string_lossy()])
            .args(["-ac", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .pre_exec(move || {
                // SAFETY: close the read end in the child so it doesn't
                // linger. The write end must stay open for Xvfb to use.
                libc::close(pipe_read_fd);
                Ok(())
            })
            .spawn()
            .context("Failed to spawn Xvfb — is Xvfb installed?")?
    };

    // Close the write end in the parent so our read sees EOF when Xvfb
    // closes its copy (either after writing the number, or on exit).
    unsafe { libc::close(pipe_write_fd) };

    let mut pipe_reader = unsafe { std::fs::File::from_raw_fd(pipe_read_fd) };
    let mut buf = String::new();
    pipe_reader
        .read_to_string(&mut buf)
        .context("Failed to read from Xvfb -displayfd pipe")?;
    drop(pipe_reader);

    if buf.trim().is_empty() {
        // Xvfb exited without writing — display was taken. Reap it.
        let mut proc = xvfb_process;
        let _ = proc.wait();
        return Ok(None);
    }

    let reported: u32 = buf
        .trim()
        .parse()
        .with_context(|| format!("Xvfb -displayfd returned non-numeric output: {:?}", buf))?;

    Ok(Some((xvfb_process, reported)))
}

/// Start Xvfb and a child application, returning a handle that owns both processes.
///
/// Display allocation uses `-displayfd` with an explicit high-base starting
/// candidate (`:100`+) to avoid colliding with real desktop X servers (Xorg,
/// Xwayland) which occupy low-numbered displays. If the candidate is taken,
/// Xvfb exits cleanly and we retry with the next number, up to
/// `DISPLAY_MAX_TRIES` attempts.
///
/// `command` is executed via `sh -c` inside the virtual display (e.g.,
/// "conky -c foo.conf"). `width` and `height` set the virtual screen
/// dimensions.
pub fn start(command: &str, width: u32, height: u32) -> Result<XvfbHandle> {
    let screen_spec = format!("{}x{}x24", width, height);

    // Scan for a free display starting from DISPLAY_BASE. Each attempt uses a
    // unique tmp_fbdir (candidate number as suffix) so concurrent calls in the
    // same process don't collide.
    let mut xvfb_process: Option<Child> = None;
    let mut display_num = 0u32;

    for candidate in DISPLAY_BASE..(DISPLAY_BASE + DISPLAY_MAX_TRIES) {
        let tmp_fbdir = std::env::temp_dir().join(format!(
            "thermalwriter-xvfb-tmp-{}-{}",
            std::process::id(),
            candidate
        ));
        std::fs::create_dir_all(&tmp_fbdir)
            .with_context(|| format!("Failed to create tmp fbdir: {}", tmp_fbdir.display()))?;

        match try_start_xvfb_on(candidate, &tmp_fbdir, &screen_spec)? {
            None => {
                // Display taken — clean up the fbdir and try the next one.
                let _ = std::fs::remove_dir_all(&tmp_fbdir);
                info!("Display :{} taken, trying :{}", candidate, candidate + 1);
                continue;
            }
            Some((proc, reported)) => {
                // Rename the tmp fbdir to the canonical numbered path.
                let fbdir =
                    std::env::temp_dir().join(format!("thermalwriter-xvfb-{}", reported));
                // Remove any stale dir from a previous run.
                let _ = std::fs::remove_dir_all(&fbdir);
                std::fs::rename(&tmp_fbdir, &fbdir).with_context(|| {
                    format!(
                        "Failed to rename fbdir {} → {}",
                        tmp_fbdir.display(),
                        fbdir.display()
                    )
                })?;
                xvfb_process = Some(proc);
                display_num = reported;
                break;
            }
        }
    }

    let xvfb_process = xvfb_process.ok_or_else(|| {
        anyhow::anyhow!(
            "No free X display found in range :{}–:{} (tried {} candidates)",
            DISPLAY_BASE,
            DISPLAY_BASE + DISPLAY_MAX_TRIES - 1,
            DISPLAY_MAX_TRIES
        )
    })?;

    let display = format!(":{}", display_num);
    let fbdir = std::env::temp_dir().join(format!("thermalwriter-xvfb-{}", display_num));
    let screen_file = fbdir.join("Xvfb_screen0");

    info!(
        "Spawned Xvfb on display {} (pid {})",
        display,
        xvfb_process.id()
    );

    // Build handle now (child_process: None) so Drop fires correctly on any
    // failure below.
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

    /// `-displayfd` with high-base retry must allocate a display in the
    /// isolated range (>= DISPLAY_BASE), keeping it clear of the live desktop.
    /// The Xvfb process is alive while the handle is held, the framebuffer
    /// screen file exists, and after Drop both the Xvfb process is dead and
    /// the fbdir has been removed.
    #[test]
    #[serial]
    fn displayfd_start_returns_valid_display() {
        let handle = start("true", 480, 480).expect("start must succeed");

        // display_num must be in the isolated range — never a low desktop display.
        assert!(
            handle.display_num() >= DISPLAY_BASE,
            "display_num {} is below DISPLAY_BASE {} — would collide with live desktop",
            handle.display_num(),
            DISPLAY_BASE
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

    /// Two sequential `start` calls must allocate distinct display numbers,
    /// both in the isolated high-base range. Both Xvfb processes must be alive
    /// while their handles are held, and both handles must drop cleanly.
    #[test]
    #[serial]
    fn displayfd_two_sequential_starts_get_distinct_displays() {
        let h1 = start("true", 480, 480).expect("first start must succeed");
        let h2 = start("true", 480, 480).expect("second start must succeed");

        let n1 = h1.display_num();
        let n2 = h2.display_num();

        // Both must be in the isolated range.
        assert!(
            n1 >= DISPLAY_BASE,
            "first display :{} is below DISPLAY_BASE {}",
            n1,
            DISPLAY_BASE
        );
        assert!(
            n2 >= DISPLAY_BASE,
            "second display :{} is below DISPLAY_BASE {}",
            n2,
            DISPLAY_BASE
        );

        assert_ne!(
            n1, n2,
            "both start() calls returned display :{} — retry-on-collision must allocate unique displays",
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
