// Service layer: daemon tick loop, D-Bus interface, and state management.
// Coordinates sensor polling, frame rendering, and USB transport.

use std::sync::Arc;

pub mod dbus;
pub mod frame_dump;
pub mod guard;
pub mod mode_handler;
pub mod tick;
pub mod xvfb;

/// One row in the live sensor catalog shared with D-Bus `list_sensors`:
/// `(key, name, unit, cost_us)`.
pub type SensorCatalogRow = (String, String, String, u64);

/// Shared live sensor catalog updated after each poll.
pub type SharedSensorCatalog = Arc<std::sync::Mutex<Vec<SensorCatalogRow>>>;
