#![cfg(all(feature = "daemon", unix))]

//! Hardware-free binary/D-Bus E2E for negotiated multi-cooler fixtures.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_thermalwriter"))
}

struct SessionBus {
    address: String,
    pid: libc::pid_t,
}

impl SessionBus {
    fn start() -> Self {
        let output = Command::new("dbus-daemon")
            .args(["--session", "--fork", "--print-address=1", "--print-pid=1"])
            .output()
            .expect("launch private dbus-daemon");
        assert!(
            output.status.success(),
            "dbus-daemon failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 dbus output");
        let mut lines = stdout.lines();
        let address = lines.next().expect("D-Bus address").to_string();
        let pid = lines
            .next()
            .expect("D-Bus pid")
            .parse::<libc::pid_t>()
            .expect("numeric D-Bus pid");
        Self { address, pid }
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        // SAFETY: pid comes directly from dbus-daemon --print-pid.
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
        }
    }
}

struct ProcessGroup {
    child: Option<Child>,
}

impl ProcessGroup {
    fn spawn(command: &mut Command) -> Self {
        // SAFETY: setpgid is async-signal-safe and does not access Rust-managed state.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        Self {
            child: Some(command.spawn().expect("spawn thermalwriter daemon")),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.as_mut().expect("child available").try_wait()
    }

    fn wait_cleanly(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.try_wait().expect("poll daemon") {
                self.child.take();
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pgid = child.id() as libc::pid_t;
        // Signal the entire process group so wrappers/descendants cannot survive
        // holding inherited descriptors or leaking between tests.
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        let _ = child.wait();
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stderr_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| format!("<failed to read stderr: {error}>"))
}

fn ctl(bus: &SessionBus, xdg: &Path, runtime: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("ctl")
        .args(args)
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_CONFIG_HOME", xdg)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .expect("run thermalwriter ctl")
}

fn wait_for_connected_status(
    group: &mut ProcessGroup,
    bus: &SessionBus,
    xdg: &Path,
    runtime: &Path,
    resolution: &str,
    stderr_path: &Path,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Some(status) = group.try_wait().expect("poll daemon") {
            panic!(
                "daemon exited before status: {status}; stderr:\n{}",
                stderr_text(stderr_path)
            );
        }
        let output = ctl(bus, xdg, runtime, &["status"]);
        last = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success()
            && last.contains("connected: true")
            && last.contains(&format!("resolution: {resolution}"))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "daemon never published connected {resolution}; last status:\n{last}\nstderr:\n{}",
        stderr_text(stderr_path)
    );
}

fn capture_sequence(path: &Path) -> Option<u64> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("frame-")?
        .parse()
        .ok()
}

fn wait_for_capture_after_until(
    dir: &Path,
    minimum_sequence: u64,
    deadline: Instant,
) -> (u64, PathBuf, PathBuf) {
    while Instant::now() < deadline {
        let mut sidecars: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml"))
            .filter_map(|path| capture_sequence(&path).map(|sequence| (sequence, path)))
            .filter(|(sequence, _)| *sequence > minimum_sequence)
            .collect();
        sidecars.sort_by_key(|(sequence, _)| *sequence);
        for (sequence, sidecar) in sidecars.into_iter().rev() {
            let payload = sidecar.with_extension("bin");
            if payload.exists() {
                return (sequence, payload, sidecar);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("expected a newer complete same-sequence .bin/.toml capture pair");
}

fn wait_for_capture_after(dir: &Path, minimum_sequence: u64) -> (u64, PathBuf, PathBuf) {
    wait_for_capture_after_until(
        dir,
        minimum_sequence,
        Instant::now() + Duration::from_secs(10),
    )
}

fn wait_for_changed_capture(
    dir: &Path,
    mut minimum_sequence: u64,
    previous_payload: &[u8],
) -> (PathBuf, PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (sequence, payload, sidecar) =
            wait_for_capture_after_until(dir, minimum_sequence, deadline);
        if std::fs::read(&payload).unwrap() != previous_payload {
            return (payload, sidecar);
        }
        minimum_sequence = sequence;
    }
}

fn assert_rgb565_be_pixel(data: &[u8], width: u32, x: u32, y: u32, expected: [u8; 2]) {
    let offset = ((y * width + x) * 2) as usize;
    assert_eq!(
        data[offset..offset + 2],
        expected,
        "unexpected RGB565-BE pixel at ({x}, {y})"
    );
}

fn run_fixture_capture(profile: &str, encoding: &str, expect_w: u32, expect_h: u32) {
    let dir = tempfile::tempdir().expect("tempdir");
    let xdg = dir.path().join("xdg");
    let capture = dir.path().join("capture");
    let runtime = dir.path().join("runtime");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    let stderr_path = dir.path().join("daemon.stderr");

    let cfg_dir = xdg.join("thermalwriter");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        r#"
[display]
tick_rate = 15
default_layout = "minimal.html"
jpeg_quality = 100
rotation = 0
mode = "svg"
device = "auto"
"#,
    )
    .unwrap();

    let bus = SessionBus::start();
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let mut command = Command::new(bin());
    command
        .arg("daemon")
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("THERMALWRITER_TRANSPORT", "null")
        .env("THERMALWRITER_PROFILE", profile)
        .env("THERMALWRITER_CAPTURE_DIR", &capture)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    let mut group = ProcessGroup::spawn(&mut command);

    wait_for_connected_status(
        &mut group,
        &bus,
        &xdg,
        &runtime,
        &format!("{expect_w}x{expect_h}"),
        &stderr_path,
    );

    let (initial_sequence, initial_bin, _) = wait_for_capture_after(&capture, 0);
    let initial_payload = std::fs::read(initial_bin).unwrap();
    let layout = ctl(&bus, &xdg, &runtime, &["layout", "svg/neon-dash-v2.svg"]);
    assert!(
        layout.status.success(),
        "set-layout failed: {}",
        String::from_utf8_lossy(&layout.stderr)
    );
    let (bin_path, toml_path) =
        wait_for_changed_capture(&capture, initial_sequence, &initial_payload);
    let status_after_switch = ctl(&bus, &xdg, &runtime, &["status"]);
    let status_text = String::from_utf8_lossy(&status_after_switch.stdout);
    assert!(
        status_after_switch.status.success()
            && status_text.contains("active_layout: svg/neon-dash-v2.svg"),
        "{status_text}"
    );

    let stop = ctl(&bus, &xdg, &runtime, &["stop"]);
    assert!(
        stop.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let status = group
        .wait_cleanly(Duration::from_secs(5))
        .unwrap_or_else(|| {
            panic!(
                "daemon did not stop cleanly; stderr:\n{}",
                stderr_text(&stderr_path)
            )
        });
    assert!(status.success(), "daemon exit status {status}");

    let meta = std::fs::read_to_string(&toml_path).unwrap();
    let expected_meta = format!(
        "profile_id = {profile:?}\nwidth = {expect_w}\nheight = {expect_h}\nencoding = {encoding:?}\n"
    );
    assert_eq!(meta, expected_meta);
    let data = std::fs::read(&bin_path).unwrap();
    if encoding == "jpeg" {
        let decoded = image::load_from_memory(&data).expect("decode captured JPEG");
        assert_eq!((decoded.width(), decoded.height()), (expect_w, expect_h));
    } else {
        assert_eq!(data.len(), expect_w as usize * expect_h as usize * 2);
        // neon-dash-v2 leaves the outer border at its deterministic #08080f
        // background. RGB565 quantization yields 0x0841, encoded big-endian.
        // Checking all four edges catches byte-order swaps and length-preserving
        // corruption of these representative pixels.
        let expected_background = [0x08, 0x41];
        for x in 0..expect_w {
            assert_rgb565_be_pixel(&data, expect_w, x, 0, expected_background);
            assert_rgb565_be_pixel(&data, expect_w, x, expect_h - 1, expected_background);
        }
        for y in 1..expect_h - 1 {
            assert_rgb565_be_pixel(&data, expect_w, 0, y, expected_background);
            assert_rgb565_be_pixel(&data, expect_w, expect_w - 1, y, expected_background);
        }
    }
}

#[test]
fn e2e_ly_fixture_publishes_switches_captures_and_stops() {
    run_fixture_capture("ly-0416-5408-pm65-sub3-fbl192", "jpeg", 1920, 462);
}

#[test]
fn e2e_scsi_fixture_publishes_switches_captures_and_stops() {
    run_fixture_capture("scsi-87cd-70db-pm100-sub0-fbl100", "rgb565-be", 320, 320);
}
