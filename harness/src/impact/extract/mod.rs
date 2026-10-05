mod paths;
mod sql;

pub use paths::{OwnerPathExtractor, TouchesExtractor};
pub use sql::{sql_accesses, SqlAccess, SqlExtractor};

use super::analyzer::Extractor;

pub(crate) fn builtin() -> Vec<Box<dyn Extractor>> {
    vec![
        Box::new(OwnerPathExtractor),
        Box::new(TouchesExtractor),
        Box::new(SqlExtractor),
    ]
}
