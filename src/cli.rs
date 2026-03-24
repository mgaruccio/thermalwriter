use std::collections::HashMap;
use std::time::{Duration, Instant};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crate::config::Config;
use crate::render::RawFrame;
use crate::service::tick::encode_jpeg;
use crate::transport::Transport;
use crate::transport::bulk_usb::BulkUsb;

/// Thermalright cooler LCD display daemon and control CLI.
#[derive(Parser, Debug)]
#[command(name = "thermalwriter", about = "Thermalright cooler LCD display daemon")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start the display daemon (USB transport + D-Bus service).
    Daemon,
    /// Control a running daemon via D-Bus.
    Ctl {
        #[command(subcommand)]
        subcommand: CtlCommand,
    },
    /// Benchmark display USB throughput.
    Bench {
        /// Duration in seconds (default: 10).
        #[arg(long, default_value_t = 10)]
        duration: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum CtlCommand {
    /// Show daemon status (active layout, connection, resolution, tick rate).
    Status,
    /// Switch the active layout.
    Layout {
        /// Layout filename (e.g. system-stats.html).
        name: String,
    },
    /// List available layouts.
    Layouts,
    /// List available sensor keys.
    Sensors,
    /// Stop the daemon.
    Stop,
    /// Reload config and reconnect.
    Reload,
}

/// zbus proxy for the com.thermalwriter.Display D-Bus interface.
#[zbus::proxy(
    interface = "com.thermalwriter.Display",
    default_service = "com.thermalwriter.Service",
    default_path = "/com/thermalwriter/display"
)]
trait Display {
    async fn get_status(&self) -> zbus::Result<HashMap<String, String>>;
    async fn set_layout(&self, name: &str) -> zbus::Result<String>;
    async fn list_layouts(&self) -> zbus::Result<Vec<String>>;
    async fn list_sensors(&self) -> zbus::Result<Vec<String>>;
    async fn stop(&self) -> zbus::Result<()>;
    async fn reload(&self) -> zbus::Result<()>;
}

/// Execute a `ctl` subcommand against the running daemon over D-Bus.
pub async fn run_ctl(cmd: CtlCommand) -> Result<()> {
    let connection = zbus::Connection::session().await
        .context("Could not connect to D-Bus session bus — is D-Bus running?")?;

    let proxy = DisplayProxy::new(&connection).await
        .context("Could not connect to thermalwriter service — is the daemon running?")?;

    match cmd {
        CtlCommand::Status => {
            let status = proxy.get_status().await
                .context("Failed to get status from daemon")?;
            // Print in sorted key order for consistent output
            let mut pairs: Vec<_> = status.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in pairs {
                println!("{}: {}", k, v);
            }
        }
        CtlCommand::Layout { name } => {
            let result = proxy.set_layout(&name).await
                .context("Failed to set layout")?;
            println!("{}", result);
        }
        CtlCommand::Layouts => {
            let layouts = proxy.list_layouts().await
                .context("Failed to list layouts")?;
            for layout in layouts {
                println!("{}", layout);
            }
        }
        CtlCommand::Sensors => {
            let sensors = proxy.list_sensors().await
                .context("Failed to list sensors")?;
            for sensor in sensors {
                println!("{}", sensor);
            }
        }
        CtlCommand::Stop => {
            proxy.stop().await
                .context("Failed to stop daemon")?;
            println!("Daemon stop signal sent.");
        }
        CtlCommand::Reload => {
            proxy.reload().await
                .context("Failed to reload daemon")?;
            println!("Daemon reload signal sent.");
        }
    }

    Ok(())
}

/// Run the USB throughput benchmark.
pub fn run_bench(duration_secs: u64) -> Result<()> {
    let config = Config::load(&Config::default_path())?;
    let quality = config.display.jpeg_quality;
    let rotation = config.display.rotation;

    // Pre-render two solid-color frames (red and blue) for visual confirmation
    let frame_red = RawFrame { data: vec![255, 0, 0].repeat(480 * 480), width: 480, height: 480 };
    let jpeg_red = encode_jpeg(&frame_red, quality, rotation)?;

    let frame_blue = RawFrame { data: vec![0, 0, 255].repeat(480 * 480), width: 480, height: 480 };
    let jpeg_blue = encode_jpeg(&frame_blue, quality, rotation)?;

    // Open USB device and handshake
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;

    println!("Benchmarking display throughput...");
    println!("  Device: {}x{}", info.width, info.height);
    println!("  Frame size: {} bytes (JPEG q={})", jpeg_red.len(), quality);
    println!("  Duration: {}s", duration_secs);
    println!();

    let duration = Duration::from_secs(duration_secs);
    let mut frame_times: Vec<Duration> = Vec::new();
    let mut use_red = true;

    let bench_start = Instant::now();
    while bench_start.elapsed() < duration {
        let frame_start = Instant::now();
        let jpeg = if use_red { &jpeg_red } else { &jpeg_blue };
        transport.send_frame(jpeg)?;
        frame_times.push(frame_start.elapsed());
        use_red = !use_red;
    }

    transport.close();

    // Report results
    let total_elapsed = bench_start.elapsed();
    let count = frame_times.len();
    if count == 0 {
        println!("No frames sent!");
        return Ok(());
    }

    let avg_fps = count as f64 / total_elapsed.as_secs_f64();
    let min = frame_times.iter().min().unwrap();
    let max = frame_times.iter().max().unwrap();
    let avg = Duration::from_nanos(
        (frame_times.iter().map(|d| d.as_nanos()).sum::<u128>() / count as u128) as u64
    );

    println!("Results:");
    println!("  Duration: {:.1}s", total_elapsed.as_secs_f64());
    println!("  Frames sent: {}", count);
    println!("  Average FPS: {:.1}", avg_fps);
    println!("  Frame time: min={:.1}ms avg={:.1}ms max={:.1}ms",
             min.as_secs_f64() * 1000.0,
             avg.as_secs_f64() * 1000.0,
             max.as_secs_f64() * 1000.0);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_daemon_subcommand() {
        let cli = Cli::try_parse_from(["thermalwriter", "daemon"]).unwrap();
        assert!(matches!(cli.command, Command::Daemon));
    }

    #[test]
    fn cli_parses_ctl_status() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "status"]).unwrap();
        assert!(matches!(cli.command, Command::Ctl { subcommand: CtlCommand::Status }));
    }

    #[test]
    fn cli_parses_ctl_layout() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "layout", "gpu-focus.html"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Ctl { subcommand: CtlCommand::Layout { ref name } } if name == "gpu-focus.html"
        ));
    }

    #[test]
    fn cli_parses_ctl_layouts() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "layouts"]).unwrap();
        assert!(matches!(cli.command, Command::Ctl { subcommand: CtlCommand::Layouts }));
    }

    #[test]
    fn cli_parses_ctl_stop() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "stop"]).unwrap();
        assert!(matches!(cli.command, Command::Ctl { subcommand: CtlCommand::Stop }));
    }

    #[test]
    fn cli_parses_ctl_reload() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "reload"]).unwrap();
        assert!(matches!(cli.command, Command::Ctl { subcommand: CtlCommand::Reload }));
    }

    #[test]
    fn cli_parses_ctl_sensors() {
        let cli = Cli::try_parse_from(["thermalwriter", "ctl", "sensors"]).unwrap();
        assert!(matches!(cli.command, Command::Ctl { subcommand: CtlCommand::Sensors }));
    }

    #[test]
    fn cli_parses_bench() {
        let cli = Cli::try_parse_from(["thermalwriter", "bench"]).unwrap();
        assert!(matches!(cli.command, Command::Bench { duration: 10 }));
    }

    #[test]
    fn cli_parses_bench_with_duration() {
        let cli = Cli::try_parse_from(["thermalwriter", "bench", "--duration", "30"]).unwrap();
        assert!(matches!(cli.command, Command::Bench { duration: 30 }));
    }

    #[test]
    fn cli_no_args_fails() {
        // No subcommand should fail (clap requires a subcommand)
        assert!(Cli::try_parse_from(["thermalwriter"]).is_err());
    }

    #[test]
    fn cli_help_text_is_valid() {
        // CommandFactory::command() builds the command — verifies clap config is correct
        let cmd = Cli::command();
        assert_eq!(cmd.get_name(), "thermalwriter");
    }
}
