pub mod background;
pub mod btop;
pub mod graph;

use tera::Tera;

/// Register all component Tera functions on a Tera instance.
pub fn register_all(tera: &mut Tera) {
    tera.register_function("background", background::BackgroundFunction);
    tera.register_function("btop_bars", btop::BtopBarsFunction);
    tera.register_function("btop_net", btop::BtopNetFunction);
    tera.register_function("btop_ram", btop::BtopRamFunction);
    tera.register_function("graph", graph::GraphFunction);
}
