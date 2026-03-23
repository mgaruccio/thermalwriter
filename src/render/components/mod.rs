pub mod graph;

use tera::Tera;

/// Register all component Tera functions on a Tera instance.
pub fn register_all(tera: &mut Tera) {
    tera.register_function("graph", graph::GraphFunction);
}
