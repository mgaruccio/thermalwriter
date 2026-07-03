// Transport layer: USB bulk transfer trait and implementations.
// Defines the Transport trait for sending frames to the cooler LCD.

pub mod bulk_usb;
pub mod null;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub width: u32,
    pub height: u32,
    pub pm: u8,
    pub sub: u8,
    pub use_jpeg: bool,
}

pub trait Transport: Send {
    /// Perform device handshake and return device info.
    fn handshake(&mut self) -> Result<DeviceInfo>;
    /// Send a frame (JPEG or RGB565 bytes depending on device).
    fn send_frame(&mut self, data: &[u8]) -> Result<()>;
    /// Release the USB device.
    fn close(&mut self);
    /// Whether the underlying device handle is currently usable.
    fn is_connected(&self) -> bool {
        true
    }
    /// Attempt to re-establish the connection (re-open + handshake), returning
    /// the negotiated device info for the new connection.
    fn try_reconnect(&mut self) -> Result<DeviceInfo> {
        anyhow::bail!("reconnect not supported by this transport")
    }
}
