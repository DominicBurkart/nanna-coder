mod paths;

pub use paths::{OwnerPathExtractor, TouchesExtractor};

use super::analyzer::Extractor;

pub(crate) fn builtin() -> Vec<Box<dyn Extractor>> {
    vec![Box::new(OwnerPathExtractor), Box::new(TouchesExtractor)]
}
