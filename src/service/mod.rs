// Service layer: daemon tick loop, D-Bus interface, and state management.
// Coordinates sensor polling, frame rendering, and USB transport.

pub mod tick;
pub mod dbus;
pub mod xvfb;
