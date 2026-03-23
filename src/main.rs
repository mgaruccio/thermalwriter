use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use clap::Parser;
use log::info;
use tokio::sync::{Mutex, watch, mpsc};

use thermalrighter::cli::{Cli, Command};
use thermalrighter::config::{Config, builtin_layouts};
use thermalrighter::sensor::SensorHub;
use thermalrighter::sensor::hwmon::HwmonProvider;
use thermalrighter::sensor::sysinfo_provider::SysinfoProvider;
use thermalrighter::sensor::amdgpu::AmdGpuProvider;
use thermalrighter::sensor::nvidia::NvidiaProvider;
use thermalrighter::sensor::mangohud::MangoHudProvider;
use thermalrighter::render::TemplateRenderer;
use thermalrighter::service::dbus::{self, ServiceState};
use thermalrighter::service::tick;
use thermalrighter::transport::Transport;
use thermalrighter::transport::bulk_usb::BulkUsb;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Ctl { subcommand } => {
            return thermalrighter::cli::run_ctl(subcommand).await;
        }
        Command::Daemon => {} // fall through to daemon startup below
    }

    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("thermalrighter");
    let layout_dir = config_dir.join("layouts");
    std::fs::create_dir_all(&layout_dir)?;

    // Load config (defaults if file missing, error if invalid TOML)
    let config_path = config_dir.join("config.toml");
    let config = Config::load(&config_path)?;
    info!("Config: tick_rate={}, layout={}, jpeg_quality={}",
          config.display.tick_rate, config.display.default_layout, config.display.jpeg_quality);

    // Seed built-in layouts on first run (only if files don't exist)
    builtin_layouts::seed_layout_dir(&layout_dir)?;

    // Load configured layout — user file in layout_dir takes precedence over built-in
    let layout_path = layout_dir.join(&config.display.default_layout);
    let template = if layout_path.exists() {
        std::fs::read_to_string(&layout_path)?
    } else {
        builtin_layouts::SYSTEM_STATS.to_string()
    };

    // Setup USB transport
    let mut transport = BulkUsb::new()?;
    let device_info = transport.handshake()?;
    info!("Device: {}x{}, PM={}, JPEG={}", device_info.width, device_info.height,
          device_info.pm, device_info.use_jpeg);

    // Setup sensor hub with all providers
    let mut sensor_hub = SensorHub::new();
    sensor_hub.add_provider(Box::new(HwmonProvider::new()));
    sensor_hub.add_provider(Box::new(SysinfoProvider::new()));
    sensor_hub.add_provider(Box::new(AmdGpuProvider::new()));
    sensor_hub.add_provider(Box::new(NvidiaProvider::new()));
    sensor_hub.add_provider(Box::new(MangoHudProvider::new()));

    // Setup template renderer
    let mut frame_source = TemplateRenderer::new(&template, device_info.width, device_info.height)?;

    // Shared state for D-Bus ↔ tick loop communication
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (layout_tx, mut layout_rx) = mpsc::channel::<String>(4);
    let (template_tx, template_rx) = watch::channel(String::new());

    let state = Arc::new(Mutex::new(ServiceState {
        active_layout: config.display.default_layout.clone(),
        connected: true,
        resolution: (device_info.width, device_info.height),
        tick_rate: config.display.tick_rate,
        jpeg_quality: config.display.jpeg_quality,
        shutdown_tx,
        layout_dir: layout_dir.clone(),
        layout_change_tx: layout_tx,
    }));

    // Start D-Bus service (connection must stay alive)
    let _connection = dbus::serve(state.clone()).await?;
    info!("D-Bus service started");

    // Layout change listener: read new layout file and push HTML to tick loop via watch channel
    let layout_dir_clone = layout_dir.clone();
    tokio::spawn(async move {
        while let Some(name) = layout_rx.recv().await {
            let path = layout_dir_clone.join(&name);
            match std::fs::read_to_string(&path) {
                Ok(html) => {
                    info!("Layout changed to: {} ({} bytes)", name, html.len());
                    if template_tx.send(html).is_err() {
                        log::warn!("Tick loop gone — layout change dropped");
                        break;
                    }
                }
                Err(e) => log::warn!("Failed to read layout {}: {}", name, e),
            }
        }
    });

    // Run tick loop — blocks until shutdown signal
    let tick_rate = state.lock().await.tick_rate;
    let jpeg_quality = state.lock().await.jpeg_quality;
    let rotation = config.display.rotation;
    tick::run_tick_loop(
        &mut transport,
        &mut frame_source,
        &mut sensor_hub,
        tick_rate,
        jpeg_quality,
        rotation,
        template_rx,
        shutdown_rx,
    ).await?;

    transport.close();
    info!("thermalrighter shutdown complete");
    Ok(())
}
