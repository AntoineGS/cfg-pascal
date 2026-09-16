pub use cfg_core;
pub use tree_sitter_pascal::LANGUAGE;

pub mod calls;
pub(crate) mod constructs;
mod exception_types;
pub mod factory;
mod pascal_builder;
pub mod project;

pub use pascal_builder::{build_file_cfgs, build_file_cfgs_in_project};
pub use project::{
    ImportBinding, ImportTarget, ProjectBuildError, ProjectSnapshot, ProjectSnapshotError,
    ProjectSourceId, ProjectUnitId, ProjectUnitInput, UsesSite,
};
