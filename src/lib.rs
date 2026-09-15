pub use cfg_core;
pub use tree_sitter_pascal::LANGUAGE;

pub mod calls;
pub(crate) mod constructs;
mod exception_types;
pub mod factory;
mod pascal_builder;

pub use pascal_builder::build_file_cfgs;
