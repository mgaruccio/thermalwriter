// Service layer: daemon tick loop, D-Bus interface, and state management.
// Coordinates sensor polling, frame rendering, and USB transport.

pub mod dbus;
pub mod frame_dump;
pub mod mode_handler;
pub mod tick;
pub mod xvfb;
