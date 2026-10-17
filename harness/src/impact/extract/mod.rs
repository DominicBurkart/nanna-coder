mod actions;
mod paths;
mod routes;
mod sql;

pub use actions::ActionExtractor;
pub use paths::{OwnerPathExtractor, TouchesExtractor};
pub use routes::{function_spans, route_bindings, ActixRouteExtractor, RouteBinding};
pub use sql::{sql_accesses, SqlAccess, SqlExtractor};

use super::analyzer::Extractor;

pub(crate) fn builtin() -> Vec<Box<dyn Extractor>> {
    vec![
        Box::new(OwnerPathExtractor),
        Box::new(TouchesExtractor),
        Box::new(SqlExtractor),
        Box::new(ActixRouteExtractor),
        Box::new(ActionExtractor),
    ]
}
