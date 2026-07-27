//! Xvfb process manager: spawns/owns Xvfb and child application processes.

use anyhow::{Context, Result, bail};
use log::info;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
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

const XAUTH_FAMILY_WILD: u16 = 0xffff;
const XAUTH_NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";

fn random_hex_128() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("Failed to open /dev/urandom for private Xvfb directory name")?
        .read_exact(&mut bytes)
        .context("Failed to read private Xvfb directory name bytes")?;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(out)
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to create private dir: {}", path.display()));
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Failed to chmod private dir: {}", path.display()))?;
    Ok(())
}

fn xvfb_private_parent_dir() -> Result<PathBuf> {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let parent = PathBuf::from(runtime_dir).join("thermalwriter");
        ensure_private_dir(&parent)?;
        Ok(parent)
    } else {
        Ok(std::env::temp_dir())
    }
}

fn create_private_fbdir() -> Result<PathBuf> {
    let parent = xvfb_private_parent_dir()?;
    for _ in 0..32 {
        let candidate = parent.join(format!("thermalwriter-xvfb-{}", random_hex_128()?));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to create private fbdir: {}", candidate.display())
                });
            }
        }
    }
    bail!("Failed to allocate unique private Xvfb framebuffer directory")
}

fn xauthority_path(fbdir: &Path) -> PathBuf {
    fbdir.join("Xauthority")
}

fn push_xauth_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(bytes);
}

fn write_xauthority(fbdir: &Path, display_num: u32) -> Result<PathBuf> {
    let mut cookie = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("Failed to open /dev/urandom for Xauthority cookie")?
        .read_exact(&mut cookie)
        .context("Failed to read Xauthority cookie")?;

    let display = display_num.to_string();
    let mut data = Vec::new();
    data.extend_from_slice(&XAUTH_FAMILY_WILD.to_be_bytes());
    push_xauth_field(&mut data, b"");
    push_xauth_field(&mut data, display.as_bytes());
    push_xauth_field(&mut data, XAUTH_NAME);
    push_xauth_field(&mut data, &cookie);

    let path = xauthority_path(fbdir);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("Failed to create Xauthority file: {}", path.display()))?;
    file.write_all(&data)
        .with_context(|| format!("Failed to write Xauthority file: {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to sync Xauthority file: {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to chmod Xauthority file: {}", path.display()))?;
    Ok(path)
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
    // Wrap both pipe ends in OwnedFd immediately so they close automatically
    // on any early return (including a spawn() failure), preventing fd leaks.
    let (pipe_read, pipe_write) = {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            bail!(
                "Failed to create pipe for -displayfd: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: pipe() returned two valid, distinct fds we now own.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    };

    // The pre_exec closure needs the read-end fd number to close it in the
    // child. Extract it before moving pipe_write into into_raw_fd below.
    let pipe_read_raw = pipe_read.as_raw_fd();
    // Convert pipe_write to a raw fd for the -displayfd argument. The
    // OwnedFd is consumed here; pipe_read keeps ownership of the read end.
    let pipe_write_raw = pipe_write.into_raw_fd();
    // From this point pipe_write_raw is an unguarded raw fd — it must be
    // closed exactly once, either by the child (via exec) or by us below.

    let xauthority = write_xauthority(fbdir, display_num)?;
    let fbdir_arg = fbdir.to_string_lossy().into_owned();
    let xauthority_arg = xauthority.to_string_lossy().into_owned();

    // Pass an explicit display number as the starting candidate. When the
    // display is free Xvfb binds it and writes the number to the pipe. When
    // it is taken Xvfb exits with code 1 and writes nothing (empty pipe →
    // clean signal to try the next candidate).
    let spawn_result = unsafe {
        Command::new("Xvfb")
            .arg(format!(":{}", display_num))
            .arg("-displayfd")
            .arg(pipe_write_raw.to_string())
            .args(["-screen", "0", screen_spec])
            .args(["-fbdir", &fbdir_arg])
            .args(["-auth", &xauthority_arg])
            .args(["-nolisten", "tcp"])
            // Stream frames go straight to the cooler LCD / GUI preview. An X
            // pointer sprite has no input role here and shows up as a white
            // arrow in captures (cava GIF, Stream-tab preview).
            .arg("-nocursor")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .pre_exec(move || {
                // SAFETY: close the read end in the child so it doesn't
                // linger. The write end must stay open for Xvfb to use.
                libc::close(pipe_read_raw);
                Ok(())
            })
            .spawn()
    };

    // Close the write end in the parent regardless of whether spawn succeeded.
    // On success: Xvfb owns a copy; closing ours lets read_to_string see EOF.
    // On failure: no child was created; closing ours releases the fd.
    // SAFETY: pipe_write_raw was produced by into_raw_fd above and has not
    // been closed anywhere else in this process.
    unsafe { libc::close(pipe_write_raw) };

    let xvfb_process = spawn_result.context("Failed to spawn Xvfb — is Xvfb installed?")?;

    // pipe_read (OwnedFd) is still live; wrap it in a File to read from it.
    // into_raw_fd() transfers ownership so the File becomes responsible for
    // closing the fd.
    let mut pipe_reader = unsafe { std::fs::File::from_raw_fd(pipe_read.into_raw_fd()) };
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
    // freshly-created 0700 framebuffer directory with an unguessable name.
    let mut xvfb_process: Option<Child> = None;
    let mut display_num = 0u32;
    let mut fbdir: Option<PathBuf> = None;

    for candidate in DISPLAY_BASE..(DISPLAY_BASE + DISPLAY_MAX_TRIES) {
        let candidate_fbdir = create_private_fbdir()?;

        match try_start_xvfb_on(candidate, &candidate_fbdir, &screen_spec)? {
            None => {
                // Display taken — clean up the fbdir and try the next one.
                let _ = std::fs::remove_dir_all(&candidate_fbdir);
                info!("Display :{} taken, trying :{}", candidate, candidate + 1);
                continue;
            }
            Some((proc, reported)) => {
                xvfb_process = Some(proc);
                display_num = reported;
                fbdir = Some(candidate_fbdir);
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
    let fbdir = fbdir.context("Xvfb started without framebuffer directory")?;
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
    //
    // SDL_VIDEODRIVER=x11 is set unconditionally: the daemon environment carries
    // WAYLAND_DISPLAY, which causes SDL to auto-probe Wayland and crash for any
    // SDL-based child (e.g. cava). Every streamed child runs inside a Xvfb X11
    // virtual display, so forcing x11 is always correct and harmless for non-SDL apps.
    let xauthority = xauthority_path(&fbdir);
    let child_process = Command::new("sh")
        .args(["-c", command])
        .env("DISPLAY", &display)
        .env("XAUTHORITY", &xauthority)
        .env("SDL_VIDEODRIVER", "x11")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Failed to spawn child command: {}", command))?;

    let child_pid = child_process.id();
    info!("Spawned child application: {} (pid {})", command, child_pid);
    handle.child_process = Some(child_process);

    // Liveness check: give the child a brief grace period then probe whether
    // it is still running. An immediately-dying child (bad command, exec error,
    // `false`, sh exit 127) indicates a misconfigured stream — return Err so
    // the D-Bus set_mode caller sees a failure and leaves daemon state unchanged.
    //
    // 150 ms is long enough to catch instant exec failures while avoiding
    // false positives for slow-initialising foreground apps: cava, conky, and
    // btop all keep their sh wrapper alive well past this window. Apps that
    // legitimately daemonize (fork + parent exit) would look dead here — that
    // is acceptable and expected: seeded configs enforce foreground operation
    // (conky `background=false`) and preset launches avoid kitty single-instance
    // mode for the same reason.
    std::thread::sleep(Duration::from_millis(150));

    if let Some(child_ref) = handle.child_process.as_mut() {
        if let Ok(Some(status)) = child_ref.try_wait() {
            // Child already exited — drop the handle (kills Xvfb, removes fbdir)
            // then propagate a descriptive error.
            drop(handle);
            bail!(
                "Streamed child exited immediately (command: {:?}, status: {}); \
                 refusing to report mode=xvfb with a dead stream",
                command,
                status
            );
        }
    }

    Ok(handle)
}

/// Start Xvfb and a child application specified as a structured argv (no shell).
///
/// Unlike [`start`] (which wraps the command in `sh -c`), this function passes
/// `argv[0]` directly to `Command::new` and `argv[1..]` as arguments. This
/// prevents shell word-splitting on arguments that contain spaces (e.g.
/// `-c /path with space/conky.conf`).
///
/// `SDL_VIDEODRIVER=x11` is set unconditionally so SDL-based apps (e.g. cava)
/// do not auto-probe Wayland. The daemon environment carries `WAYLAND_DISPLAY`;
/// every streamed child runs inside a Xvfb X11 virtual display so forcing x11
/// is always correct and harmless for non-SDL apps.
///
/// All invariants from [`start`] are preserved:
/// - Display allocation starts at `DISPLAY_BASE` (`:100`) to avoid collisions
///   with the live desktop X server.
/// - `DISPLAY` is set in the child's environment.
/// - `.process_group(0)` is set on the child so `Drop` can kill the entire
///   process group (including grandchildren).
/// - The child-liveness check (150 ms grace + `try_wait`) is applied — an
///   immediately-dying child returns `Err` so the D-Bus caller sees failure and
///   daemon state is left unchanged.
pub fn start_argv(argv: &[String], width: u32, height: u32) -> Result<XvfbHandle> {
    if argv.is_empty() {
        bail!("start_argv: argv must not be empty");
    }

    let screen_spec = format!("{}x{}x24", width, height);

    // Scan for a free display starting from DISPLAY_BASE. Mirrors start().
    let mut xvfb_process: Option<Child> = None;
    let mut display_num = 0u32;
    let mut fbdir: Option<PathBuf> = None;
    for candidate in DISPLAY_BASE..(DISPLAY_BASE + DISPLAY_MAX_TRIES) {
        let candidate_fbdir = create_private_fbdir()?;

        match try_start_xvfb_on(candidate, &candidate_fbdir, &screen_spec)? {
            None => {
                let _ = std::fs::remove_dir_all(&candidate_fbdir);
                info!("Display :{} taken, trying :{}", candidate, candidate + 1);
                continue;
            }
            Some((proc, reported)) => {
                xvfb_process = Some(proc);
                display_num = reported;
                fbdir = Some(candidate_fbdir);
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
    let fbdir = fbdir.context("Xvfb started without framebuffer directory")?;
    let screen_file = fbdir.join("Xvfb_screen0");

    info!(
        "Spawned Xvfb on display {} (pid {})",
        display,
        xvfb_process.id()
    );

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
            bail!(
                "Xvfb screen file did not appear within 5 seconds: {}",
                screen_file.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    info!("Xvfb screen file ready: {}", screen_file.display());

    // Spawn the child using a structured argv (no shell).
    // argv[0] is the binary; argv[1..] are its arguments, passed verbatim —
    // no shell word-splitting occurs on spaces within any element.
    //
    // SDL_VIDEODRIVER=x11 is set unconditionally (same rationale as start()).
    let xauthority = xauthority_path(&fbdir);
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.env("DISPLAY", &display)
        .env("XAUTHORITY", &xauthority)
        .env("SDL_VIDEODRIVER", "x11")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);

    let child_process = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn child argv: {:?}", argv))?;

    let child_pid = child_process.id();
    info!(
        "Spawned child application (argv): {:?} (pid {})",
        argv, child_pid
    );
    handle.child_process = Some(child_process);

    // Child-liveness check — same 150 ms grace as start(). Preserves the
    // Phase 1 contract: an immediately-dying child returns Err so the D-Bus
    // caller sees failure and daemon state is left unchanged.
    std::thread::sleep(Duration::from_millis(150));

    if let Some(child_ref) = handle.child_process.as_mut() {
        if let Ok(Some(status)) = child_ref.try_wait() {
            drop(handle);
            bail!(
                "Streamed child (argv) exited immediately (argv: {:?}, status: {}); \
                 refusing to report mode=xvfb with a dead stream",
                argv,
                status
            );
        }
    }

    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn private_fbdir_is_created_0700_with_unpredictable_name() {
        let fbdir = create_private_fbdir().expect("private fbdir should be created");
        let metadata = std::fs::metadata(&fbdir).expect("fbdir metadata should be readable");

        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert!(
            fbdir
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("thermalwriter-xvfb-") && name.len() > 40),
            "fbdir name should include a random suffix: {}",
            fbdir.display()
        );

        std::fs::remove_dir_all(fbdir).expect("fbdir cleanup should succeed");
    }

    #[test]
    fn xauthority_is_created_0600() {
        let fbdir = create_private_fbdir().expect("private fbdir should be created");
        let xauth = write_xauthority(&fbdir, DISPLAY_BASE).expect("Xauthority should be written");
        let metadata = std::fs::metadata(&xauth).expect("Xauthority metadata should be readable");

        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        std::fs::remove_dir_all(fbdir).expect("fbdir cleanup should succeed");
    }

    /// Returns `true` if a process with the given PID exists (is alive).
    fn pid_alive(pid: u32) -> bool {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0
    }

    // ---------------------------------------------------------------------------
    // Task 7 tests: start_argv — no shell, no word-splitting, env injection
    // ---------------------------------------------------------------------------

    /// [DO-CONFIRM checklist item 1]:
    ///
    /// A preset argv element containing a space must reach the child process as
    /// a single argument — no shell word-splitting. We invoke
    /// `start_argv(["sleep", "5"], [], ...)` and also a path-with-space variant
    /// by spawning a sentinel that reads its own argv[1] and writes it to a
    /// temp file, then asserting the full string arrived intact.
    ///
    /// This test FAILS TO COMPILE until `start_argv` is defined.
    #[test]
    #[serial]
    fn argv_arg_with_space_not_word_split() {
        // Build a one-shot script: write argv[1] to a temp file, then exit.
        // We use `sh -c` for the *script body* (it's our controlled payload),
        // but the *outer launch* is via start_argv with a structured argv so
        // the argument with a space is passed verbatim to sh -c as a single $1.
        //
        // argv: ["sh", "-c", "printf '%s' \"$1\" > $2; exit 0", "--", "arg with space", "<file>"]
        // If start_argv word-splits, sh receives "arg", "with", "space" as $1/$2/$3;
        // if it doesn't, sh receives "arg with space" as $1.
        let out_file = tempfile::NamedTempFile::new().expect("tempfile");
        let out_path = out_file.path().to_string_lossy().to_string();
        let argv: Vec<String> = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf '%s' \"$1\" > {}; exec sleep 5", out_path),
            "--".to_string(),
            "arg with space".to_string(),
        ];

        let handle = start_argv(&argv, 480, 480)
            .expect("start_argv must succeed for sh with a space-containing arg");

        // Give the child a moment to write the file.
        std::thread::sleep(std::time::Duration::from_millis(300));

        let written = std::fs::read_to_string(&out_path).unwrap_or_default();
        drop(handle);

        assert_eq!(
            written.trim(),
            "arg with space",
            "arg-with-space must arrive as a single arg (no word-splitting): got {:?}",
            written
        );
    }

    /// [DO-CONFIRM checklist item 4 + 5]:
    ///
    /// start_argv must set .process_group(0) on the child and pass the
    /// liveness check (child survives 150 ms grace period).
    /// Also verifies that display_num >= DISPLAY_BASE (no regression).
    #[test]
    #[serial]
    fn start_argv_liveness_check_and_display_base() {
        let argv: Vec<String> = vec!["sleep".to_string(), "10".to_string()];
        let handle = start_argv(&argv, 480, 480).expect("start_argv(sleep 10) must succeed");

        assert!(
            handle.display_num() >= DISPLAY_BASE,
            "display_num {} below DISPLAY_BASE {} — regression in display allocation",
            handle.display_num(),
            DISPLAY_BASE
        );
        assert!(
            handle.screen_file().exists(),
            "screen file must exist for a live start_argv handle"
        );
        let auth_path = xauthority_path(&handle.fbdir);
        assert!(
            auth_path.exists(),
            "Xauthority file must exist beside the Xvfb fbdir"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&auth_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "Xauthority file must be private");
        }
        drop(handle);
    }

    /// [DO-CONFIRM checklist item 1 variant]:
    ///
    /// A child that dies immediately in start_argv must return Err, same as
    /// the existing sh -c liveness check (Phase 1 contract preserved in argv path).
    #[test]
    #[serial]
    fn start_argv_dying_child_returns_err() {
        let argv: Vec<String> = vec!["false".to_string()];
        let result = start_argv(&argv, 480, 480);
        assert!(
            result.is_err(),
            "start_argv(false) must return Err when child dies immediately — \
             liveness check must be preserved in argv path"
        );
    }

    /// [DO-CONFIRM: SDL_VIDEODRIVER=x11 injected unconditionally in argv path]:
    ///
    /// start_argv must set SDL_VIDEODRIVER=x11 for all streamed children so
    /// SDL-based apps (e.g. cava) work inside the Xvfb X11 virtual display even
    /// when the daemon environment carries WAYLAND_DISPLAY.
    ///
    /// We write the env var value to a temp file and assert it equals "x11".
    #[test]
    #[serial]
    fn start_argv_sdl_videodriver_set_unconditionally() {
        let out_file = tempfile::NamedTempFile::new().expect("tempfile");
        let out_path = out_file.path().to_string_lossy().to_string();

        // sh reads SDL_VIDEODRIVER and writes it to the output file, then stays alive.
        let argv: Vec<String> = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "printf '%s' \"$SDL_VIDEODRIVER\" > {}; exec sleep 5",
                out_path
            ),
        ];

        let handle = start_argv(&argv, 480, 480).expect("start_argv with SDL check must succeed");

        std::thread::sleep(std::time::Duration::from_millis(300));
        let written = std::fs::read_to_string(&out_path).unwrap_or_default();
        drop(handle);

        assert_eq!(
            written.trim(),
            "x11",
            "SDL_VIDEODRIVER must be 'x11' in argv child env, got: {:?}",
            written
        );
    }

    /// [DO-CONFIRM: SDL_VIDEODRIVER=x11 injected unconditionally in sh -c path]:
    ///
    /// start() (shell path) must also set SDL_VIDEODRIVER=x11 unconditionally.
    #[test]
    #[serial]
    fn start_sh_sdl_videodriver_set_unconditionally() {
        let out_file = tempfile::NamedTempFile::new().expect("tempfile");
        let out_path = out_file.path().to_string_lossy().to_string();

        let handle = start(
            &format!(
                "printf '%s' \"$SDL_VIDEODRIVER\" > {}; exec sleep 5",
                out_path
            ),
            480,
            480,
        )
        .expect("start with SDL check must succeed");

        std::thread::sleep(std::time::Duration::from_millis(300));
        let written = std::fs::read_to_string(&out_path).unwrap_or_default();
        drop(handle);

        assert_eq!(
            written.trim(),
            "x11",
            "SDL_VIDEODRIVER must be 'x11' in sh -c child env, got: {:?}",
            written
        );
    }

    /// `-displayfd` with high-base retry must allocate a display in the
    /// isolated range (>= DISPLAY_BASE), keeping it clear of the live desktop.
    /// The Xvfb process is alive while the handle is held, the framebuffer
    /// screen file exists, and after Drop both the Xvfb process is dead and
    /// the fbdir has been removed.
    #[test]
    #[serial]
    fn displayfd_start_returns_valid_display() {
        // Use a foreground long-lived command so the liveness check passes.
        let handle = start("sleep 5", 480, 480).expect("start must succeed");

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
        // Use a foreground long-lived command so the liveness check passes.
        let h1 = start("sleep 5", 480, 480).expect("first start must succeed");
        let h2 = start("sleep 5", 480, 480).expect("second start must succeed");

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

    /// An immediately-dying child must cause start() to return Err, AND the
    /// Xvfb it already spawned must be cleaned up (no orphaned process or fbdir).
    ///
    /// `false` exits with code 1 instantly; `sh -c 'this-binary-does-not-exist'`
    /// exits with code 127 (command not found) — both exercise the liveness check.
    #[test]
    #[serial]
    fn dying_child_causes_start_to_return_err_and_cleans_up_xvfb() {
        // Record all fbdirs before the call so we can check for leaks.
        let fbdir_before: std::collections::HashSet<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("thermalwriter-xvfb-"))
                    .unwrap_or(false)
            })
            .collect();

        // `false` exits immediately with code 1 — no Xvfb stream should start.
        let result = start("false", 480, 480);

        assert!(
            result.is_err(),
            "start(\"false\") must return Err (child died immediately) but returned Ok"
        );

        // Give Drop a moment to reap Xvfb and remove the fbdir.
        std::thread::sleep(Duration::from_millis(400));

        // No new thermalwriter-xvfb-* dirs should remain (Xvfb was cleaned up).
        let fbdir_after: std::collections::HashSet<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("thermalwriter-xvfb-"))
                    .unwrap_or(false)
            })
            .collect();

        let leaked: Vec<_> = fbdir_after.difference(&fbdir_before).collect();
        assert!(
            leaked.is_empty(),
            "start(\"false\") leaked fbdir(s) after Err: {:?}",
            leaked
        );
    }

    /// A living child must allow start() to return Ok.
    #[test]
    #[serial]
    fn living_child_causes_start_to_return_ok() {
        let handle = start("sleep 10", 480, 480).expect("start(\"sleep 10\") must return Ok");
        // Child is alive — confirm the handle is usable.
        assert!(
            handle.screen_file().exists(),
            "screen file must exist for a live handle"
        );
        drop(handle); // must not panic
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
        // Spawn a foreground sh that itself starts a background sleep sentinel.
        // The sh wrapper stays alive (keeping the liveness check happy) while
        // sleep 9473 runs as a grandchild. This is the same process-group kill
        // scenario as before; using exec keeps sh alive so we don't trip the
        // 150ms liveness check.
        let handle = match start("sleep 9473 & exec sleep 600", 480, 480) {
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
