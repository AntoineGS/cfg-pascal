//! Immutable caller-owned inputs for project-aware CFG construction.
//!
//! A [`ProjectSnapshot`] owns the parsed unit inputs and the explicit
//! selections made for each `uses` entry.  It deliberately does not discover
//! files or infer imports from names.  Callers can therefore build a snapshot
//! from an editor/LSP project model without giving this crate filesystem or
//! process access.

use std::{collections::HashSet, fmt, ops::Range, sync::Arc};

use tree_sitter::{Node, Tree};

/// Stable caller-assigned identity for one logical parsed Pascal unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectUnitId(String);

impl ProjectUnitId {
    /// Create an identity.  Empty identities are rejected when the snapshot
    /// is constructed so callers can conveniently build inputs first.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Return the caller-supplied identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProjectUnitId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ProjectUnitId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// Stable caller-assigned identity for the exact source snapshot paired with
/// a parsed unit tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectSourceId(String);

impl ProjectSourceId {
    /// Create an identity.  Empty identities are rejected when the snapshot
    /// is constructed.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Return the caller-supplied identity text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProjectSourceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ProjectSourceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// One parsed unit and the exact source bytes from which its tree was made.
///
/// The fields are private so a constructed [`ProjectSnapshot`] cannot be
/// changed behind the resolver's back.  The source/tree pairing is a caller
/// contract: this type validates byte bounds, but tree-sitter does not expose
/// the bytes originally used to create a [`Tree`] and therefore cannot prove
/// equality with a separately supplied byte slice.
#[derive(Debug, Clone)]
pub struct ProjectUnitInput {
    id: ProjectUnitId,
    source_id: ProjectSourceId,
    tree: Tree,
    source: Arc<[u8]>,
}

impl ProjectUnitInput {
    /// Construct a parsed unit input from one tree/source snapshot.
    pub fn new(
        id: ProjectUnitId,
        source_id: ProjectSourceId,
        tree: Tree,
        source: impl AsRef<[u8]>,
    ) -> Self {
        Self {
            id,
            source_id,
            tree,
            source: Arc::from(source.as_ref()),
        }
    }

    /// Stable logical unit identity.
    pub fn id(&self) -> &ProjectUnitId {
        &self.id
    }

    /// Stable identity of the exact source snapshot.
    pub fn source_id(&self) -> &ProjectSourceId {
        &self.source_id
    }

    /// Borrow the immutable parsed tree.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Borrow the immutable source bytes paired with [`Self::tree`].
    pub fn source(&self) -> &[u8] {
        &self.source
    }
}

/// A source span identifying one named unit in a `uses` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsesSite {
    unit_id: ProjectUnitId,
    byte_range: Range<usize>,
}

impl UsesSite {
    /// Construct a uses-entry key.  The range must exactly identify a
    /// `moduleName` node within a `declUses` node in the selected unit.
    pub fn new(unit_id: ProjectUnitId, byte_range: Range<usize>) -> Self {
        Self {
            unit_id,
            byte_range,
        }
    }

    /// Unit containing the uses entry.
    pub fn unit_id(&self) -> &ProjectUnitId {
        &self.unit_id
    }

    /// Exact source range of the uses entry.
    pub fn byte_range(&self) -> Range<usize> {
        self.byte_range.clone()
    }
}

/// The project-model result for one uses entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// The target is present in this immutable snapshot.
    Loaded(ProjectUnitId),
    /// Resolution was attempted but no unique target was available.
    Unavailable,
    /// More than one project target matched and the caller did not choose one.
    Ambiguous,
}

/// Explicit target and qualifier authorization for one uses entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBinding {
    site: UsesSite,
    target: ImportTarget,
    authorized_qualifiers: Vec<String>,
}

impl ImportBinding {
    /// Construct an explicit import selection.
    ///
    /// `authorized_qualifiers` contains the spellings that may qualify names
    /// from the selected target, for example `"Vendor.Errors"` and
    /// `"Errors"`.  An empty list permits only unqualified lookup through the
    /// uses entry.  No qualifier is inferred from a unit ID or module name.
    pub fn new<I, Q>(site: UsesSite, target: ImportTarget, authorized_qualifiers: I) -> Self
    where
        I: IntoIterator<Item = Q>,
        Q: Into<String>,
    {
        Self {
            site,
            target,
            authorized_qualifiers: authorized_qualifiers.into_iter().map(Into::into).collect(),
        }
    }

    /// Uses-entry key for this binding.
    pub fn site(&self) -> &UsesSite {
        &self.site
    }

    /// Selected project target.
    pub fn target(&self) -> &ImportTarget {
        &self.target
    }

    /// Qualifiers explicitly authorized for this target.
    pub fn authorized_qualifiers(&self) -> &[String] {
        &self.authorized_qualifiers
    }
}

/// Validation failures found while constructing an immutable project
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSnapshotError {
    /// A unit ID was empty.
    EmptyUnitId,
    /// A source ID was empty.
    EmptySourceId,
    /// Two inputs used the same logical unit ID.
    DuplicateUnitId(ProjectUnitId),
    /// Two inputs claimed the same source identity.
    DuplicateSourceId(ProjectSourceId),
    /// A tree's root byte range cannot be represented by its paired source.
    InvalidTreeSource {
        unit_id: ProjectUnitId,
        tree_range: Range<usize>,
        source_len: usize,
    },
    /// A binding site is not an exact named unit in a `uses` clause, or its
    /// unit/range cannot be found in the snapshot.
    InvalidImportSite {
        unit_id: ProjectUnitId,
        byte_range: Range<usize>,
    },
    /// The same uses-entry key was selected more than once.
    DuplicateImportSite {
        unit_id: ProjectUnitId,
        byte_range: Range<usize>,
    },
    /// A loaded target ID is absent from the snapshot.
    DanglingLoadedTarget {
        importer: ProjectUnitId,
        target: ProjectUnitId,
    },
    /// A qualifier is empty or contains an empty dotted component.
    InvalidQualifier(String),
}

impl fmt::Display for ProjectSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUnitId => formatter.write_str("project unit ID must not be empty"),
            Self::EmptySourceId => formatter.write_str("project source ID must not be empty"),
            Self::DuplicateUnitId(id) => write!(formatter, "duplicate project unit ID {:?}", id),
            Self::DuplicateSourceId(id) => {
                write!(formatter, "duplicate project source ID {:?}", id)
            }
            Self::InvalidTreeSource {
                unit_id,
                tree_range,
                source_len,
            } => write!(
                formatter,
                "tree range {:?} for unit {:?} exceeds source length {}",
                tree_range, unit_id, source_len
            ),
            Self::InvalidImportSite {
                unit_id,
                byte_range,
            } => write!(
                formatter,
                "import site {:?} is not a uses entry in unit {:?}",
                byte_range, unit_id
            ),
            Self::DuplicateImportSite {
                unit_id,
                byte_range,
            } => write!(
                formatter,
                "duplicate import site {:?} in unit {:?}",
                byte_range, unit_id
            ),
            Self::DanglingLoadedTarget { importer, target } => write!(
                formatter,
                "import from unit {:?} targets missing loaded unit {:?}",
                importer, target
            ),
            Self::InvalidQualifier(qualifier) => {
                write!(formatter, "invalid authorized qualifier {:?}", qualifier)
            }
        }
    }
}

impl std::error::Error for ProjectSnapshotError {}

/// Failures that can occur when selecting a unit to build from a valid
/// project snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectBuildError {
    /// The requested stable unit ID is not part of the snapshot.
    UnitNotFound(ProjectUnitId),
}

impl fmt::Display for ProjectBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnitNotFound(id) => write!(formatter, "project unit {:?} was not found", id),
        }
    }
}

impl std::error::Error for ProjectBuildError {}

/// An immutable set of parsed units and occurrence-specific import choices.
#[derive(Debug, Clone)]
pub struct ProjectSnapshot {
    units: Vec<ProjectUnitInput>,
    imports: Vec<ImportBinding>,
}

impl ProjectSnapshot {
    /// Validate and freeze parsed units plus explicit uses-entry selections.
    ///
    /// Missing bindings are permitted because a project may intentionally
    /// omit an external or unresolved unit.  Such an omitted entry is treated
    /// conservatively as an unknown namespace; it is never discovered by
    /// matching a name suffix.
    pub fn new(
        units: Vec<ProjectUnitInput>,
        imports: Vec<ImportBinding>,
    ) -> Result<Self, ProjectSnapshotError> {
        let mut unit_ids = HashSet::new();
        let mut source_ids = HashSet::new();

        for unit in &units {
            if unit.id.as_str().is_empty() {
                return Err(ProjectSnapshotError::EmptyUnitId);
            }
            if unit.source_id.as_str().is_empty() {
                return Err(ProjectSnapshotError::EmptySourceId);
            }
            if !unit_ids.insert(unit.id.clone()) {
                return Err(ProjectSnapshotError::DuplicateUnitId(unit.id.clone()));
            }
            if !source_ids.insert(unit.source_id.clone()) {
                return Err(ProjectSnapshotError::DuplicateSourceId(
                    unit.source_id.clone(),
                ));
            }

            let root = unit.tree.root_node();
            if root.start_byte() != 0 || root.end_byte() > unit.source.len() {
                return Err(ProjectSnapshotError::InvalidTreeSource {
                    unit_id: unit.id.clone(),
                    tree_range: root.start_byte()..root.end_byte(),
                    source_len: unit.source.len(),
                });
            }
        }

        let mut import_sites = HashSet::new();
        for import in &imports {
            let unit_id = import.site.unit_id();
            let Some(unit) = units.iter().find(|unit| unit.id == *unit_id) else {
                return Err(ProjectSnapshotError::InvalidImportSite {
                    unit_id: unit_id.clone(),
                    byte_range: import.site.byte_range(),
                });
            };
            let byte_range = import.site.byte_range();
            if !import_site_spans(unit.tree.root_node())
                .contains(&(byte_range.start, byte_range.end))
            {
                return Err(ProjectSnapshotError::InvalidImportSite {
                    unit_id: unit_id.clone(),
                    byte_range,
                });
            }
            if !import_sites.insert((unit_id.clone(), byte_range.start, byte_range.end)) {
                return Err(ProjectSnapshotError::DuplicateImportSite {
                    unit_id: unit_id.clone(),
                    byte_range,
                });
            }

            if let ImportTarget::Loaded(target) = import.target() {
                if !unit_ids.contains(target) {
                    return Err(ProjectSnapshotError::DanglingLoadedTarget {
                        importer: unit_id.clone(),
                        target: target.clone(),
                    });
                }
            }

            for qualifier in import.authorized_qualifiers() {
                if !valid_qualifier(qualifier) {
                    return Err(ProjectSnapshotError::InvalidQualifier(qualifier.clone()));
                }
            }
        }

        Ok(Self { units, imports })
    }

    /// Borrow all immutable parsed units in caller order.
    pub fn units(&self) -> &[ProjectUnitInput] {
        &self.units
    }

    /// Borrow all explicit import selections in caller order.
    pub fn imports(&self) -> &[ImportBinding] {
        &self.imports
    }

    /// Find a parsed unit by its stable caller-assigned ID.
    pub fn unit(&self, id: &ProjectUnitId) -> Option<&ProjectUnitInput> {
        self.units.iter().find(|unit| unit.id == *id)
    }
}

/// Return the exact source spans accepted as [`UsesSite`] keys.
pub(crate) fn import_site_spans(root: Node<'_>) -> HashSet<(usize, usize)> {
    fn collect(node: Node<'_>, in_uses: bool, spans: &mut HashSet<(usize, usize)>) {
        let in_uses = in_uses || node.kind() == "declUses";
        if in_uses && node.kind() == "moduleName" {
            spans.insert((node.start_byte(), node.end_byte()));
            return;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect(child, in_uses, spans);
        }
    }

    let mut spans = HashSet::new();
    collect(root, false, &mut spans);
    spans
}

fn valid_qualifier(qualifier: &str) -> bool {
    !qualifier.is_empty()
        && qualifier
            .split('.')
            .all(|component| !component.is_empty() && !component.chars().any(char::is_whitespace))
}
