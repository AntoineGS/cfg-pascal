//! Strict, immutable prepared-source inputs for project-aware CFG builds.
//!
//! Preparation itself is intentionally not implemented here.  A caller (for
//! example, a future project/configuration service) supplies the projected
//! bytes and validated [`SourceMap`].  This layer records the caller's
//! configuration/provenance assertion, parses with this crate's Pascal
//! language, and refuses unresolved or lossy input before it can be treated as
//! a precise project unit.

use std::{fmt, ops::Range, sync::Arc};

use tree_sitter::{Parser, Tree};

use crate::{
    source_map::{MappedSourceSpan, SourceMap, SourceMapError, SourceSnapshot},
    ProjectSourceId,
};

/// Caller-supplied fidelity assertion for a prepared projection.
///
/// `Complete` means that the caller explicitly asserts that all executable
/// content relevant to the projection is represented.  It is not inferred
/// from the absence of preprocessor nodes and is not a proof that conditional
/// expressions were semantically evaluated correctly.  `Unresolved` and
/// `Lossy` are retained as explicit states so incomplete work cannot be
/// accidentally promoted to a precise [`PreparedSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreparationFidelity {
    /// The caller explicitly asserts a complete executable projection.
    Complete,
    /// Some active content or configuration is unresolved.
    Unresolved,
    /// Content was dropped or changed without a complete source contract.
    Lossy,
    /// The projection is known to be incomplete but is not classified more
    /// specifically.  This is also rejected by strict preparation.
    Incomplete,
}

impl PreparationFidelity {
    /// Alias emphasizing that `Complete` is an explicit caller assertion.
    pub const EXPLICIT_COMPLETE: Self = Self::Complete;

    /// Whether this fidelity is accepted by [`PreparedSource::new`].
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Broad origin of a prepared source.
///
/// This is metadata only.  `Configured` does not claim compiler-complete
/// semantics; the caller's [`PreparationFidelity`] is the separate explicit
/// completeness assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreparationProvenance {
    /// The prepared bytes are an exact raw identity projection.
    Raw,
    /// The prepared bytes came from an explicitly selected configuration or
    /// include projection.
    Configured,
}

impl PreparationProvenance {
    /// Alias for the raw identity provenance spelling.
    pub const RAW_IDENTITY: Self = Self::Raw;
}

/// Errors raised while constructing a strict [`PreparedSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedSourceError {
    /// The prepared source identity was empty.
    EmptySourceId(ProjectSourceId),
    /// The configuration identity was empty.
    EmptyConfigurationId,
    /// The caller did not assert complete prepared content.
    RejectedFidelity(PreparationFidelity),
    /// The map was validated against bytes with a different length.
    PreparedLengthMismatch { map_len: usize, prepared_len: usize },
    /// The map was validated against different bytes of the same length.
    PreparedBytesMismatch,
    /// The source map was invalid.
    SourceMap(SourceMapError),
    /// The parser returned no tree.
    ParserReturnedNoTree,
    /// The prepared bytes produced a tree containing parser errors.
    ParserErrors { range: Range<usize> },
    /// The raw identity convenience path found an unresolved preprocessor
    /// node.  Callers that have resolved the directive must use
    /// [`PreparedSource::new`] with the resulting prepared bytes and source
    /// map instead.
    UnresolvedPreprocessor {
        /// Byte range of the first preprocessor node.
        range: Range<usize>,
        /// Tree-sitter kind of the preprocessor node.
        node_kind: String,
    },
}

impl fmt::Display for PreparedSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourceId(id) => {
                write!(formatter, "prepared source ID {:?} must not be empty", id)
            }
            Self::EmptyConfigurationId => {
                formatter.write_str("prepared source configuration ID must not be empty")
            }
            Self::RejectedFidelity(fidelity) => write!(
                formatter,
                "prepared source requires an explicit complete projection, got {fidelity:?}"
            ),
            Self::PreparedLengthMismatch {
                map_len,
                prepared_len,
            } => write!(
                formatter,
                "source map prepared length {} does not match prepared length {}",
                map_len, prepared_len
            ),
            Self::PreparedBytesMismatch => formatter.write_str(
                "source map was validated against different prepared bytes of the same length",
            ),
            Self::SourceMap(error) => write!(formatter, "invalid prepared source map: {error}"),
            Self::ParserReturnedNoTree => formatter.write_str("Pascal parser returned no tree"),
            Self::ParserErrors { range } => {
                write!(
                    formatter,
                    "prepared Pascal source contains parser errors in {range:?}"
                )
            }
            Self::UnresolvedPreprocessor { range, node_kind } => write!(
                formatter,
                "raw identity preparation cannot claim completeness for {node_kind:?} in {range:?}"
            ),
        }
    }
}

impl std::error::Error for PreparedSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceMap(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SourceMapError> for PreparedSourceError {
    fn from(error: SourceMapError) -> Self {
        Self::SourceMap(error)
    }
}

/// An immutable, parse-clean source projection with its original-source map.
///
/// The `source_id` identifies the exact prepared buffer consumed by the CFG
/// builder.  Original files and include occurrences are available through
/// [`Self::source_map`] and [`Self::original_sources`].  The parsed tree and
/// bytes are created and owned together, so a project snapshot cannot observe
/// a later caller mutation.
#[derive(Debug, Clone)]
pub struct PreparedSource {
    source_id: ProjectSourceId,
    bytes: Arc<[u8]>,
    tree: Tree,
    source_map: SourceMap,
    configuration_id: String,
    fidelity: PreparationFidelity,
    provenance: PreparationProvenance,
}

impl PreparedSource {
    /// Parse and validate a prepared source projection.
    ///
    /// The parser always uses [`crate::LANGUAGE`].  A source map must already
    /// be validated against the exact same prepared bytes; both its length
    /// and bytes are checked again here.  Only [`PreparationFidelity::Complete`]
    /// is accepted, so missing or lossy executable content cannot become an
    /// apparently empty include or a precise CFG by accident.
    ///
    /// ```rust
    /// use cfg_pascal::{
    ///     PreparationFidelity, PreparationProvenance, PreparedSource,
    ///     ProjectSnapshot, ProjectSourceId, ProjectUnitId, ProjectUnitInput,
    ///     SourceMap, SourceSnapshot,
    /// };
    ///
    /// let bytes = b"unit Demo; interface implementation end.";
    /// let map = SourceMap::identity(SourceSnapshot::new(
    ///     ProjectSourceId::from("demo.pas"),
    ///     bytes,
    /// ))?;
    /// let prepared = PreparedSource::new(
    ///     ProjectSourceId::from("demo.prepared"),
    ///     bytes,
    ///     map,
    ///     "debug",
    ///     PreparationFidelity::Complete,
    ///     PreparationProvenance::Configured,
    /// )?;
    /// let unit = ProjectUnitInput::from_prepared(ProjectUnitId::from("demo"), prepared);
    /// let snapshot = ProjectSnapshot::new(vec![unit], Vec::new())?;
    /// assert_eq!(snapshot.configuration_id(), Some("debug"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(
        source_id: ProjectSourceId,
        prepared_bytes: impl AsRef<[u8]>,
        source_map: SourceMap,
        configuration_id: impl Into<String>,
        fidelity: PreparationFidelity,
        provenance: PreparationProvenance,
    ) -> Result<Self, PreparedSourceError> {
        let prepared_bytes = prepared_bytes.as_ref();
        if source_id.as_str().is_empty() {
            return Err(PreparedSourceError::EmptySourceId(source_id));
        }

        let configuration_id = configuration_id.into();
        if configuration_id.is_empty() {
            return Err(PreparedSourceError::EmptyConfigurationId);
        }
        if !fidelity.is_complete() {
            return Err(PreparedSourceError::RejectedFidelity(fidelity));
        }
        if source_map.prepared_len() != prepared_bytes.len() {
            return Err(PreparedSourceError::PreparedLengthMismatch {
                map_len: source_map.prepared_len(),
                prepared_len: prepared_bytes.len(),
            });
        }
        if source_map.prepared_bytes() != prepared_bytes {
            return Err(PreparedSourceError::PreparedBytesMismatch);
        }

        let mut parser = Parser::new();
        parser
            .set_language(&crate::LANGUAGE.into())
            .map_err(|_| PreparedSourceError::ParserReturnedNoTree)?;
        let tree = parser
            .parse(prepared_bytes, None)
            .ok_or(PreparedSourceError::ParserReturnedNoTree)?;
        let root = tree.root_node();
        if root.has_error() {
            return Err(PreparedSourceError::ParserErrors {
                range: root.start_byte()..root.end_byte(),
            });
        }

        Ok(Self {
            source_id,
            bytes: source_map.prepared_bytes_arc(),
            tree,
            source_map,
            configuration_id,
            fidelity,
            provenance,
        })
    }

    /// Build a prepared source directly from original snapshots and ordered
    /// segments.  This is the intended seam for a future pure configurator.
    pub fn from_segments(
        source_id: ProjectSourceId,
        prepared_bytes: impl AsRef<[u8]>,
        original_sources: Vec<SourceSnapshot>,
        segments: Vec<crate::SourceMapSegment>,
        configuration_id: impl Into<String>,
        fidelity: PreparationFidelity,
        provenance: PreparationProvenance,
    ) -> Result<Self, PreparedSourceError> {
        let source_map = SourceMap::new(&prepared_bytes, original_sources, segments)?;
        Self::new(
            source_id,
            prepared_bytes,
            source_map,
            configuration_id,
            fidelity,
            provenance,
        )
    }

    /// Construct a complete raw identity projection.
    ///
    /// This convenience path is valid only for a parse-clean source without
    /// any preprocessor nodes.  An unresolved include or conditional directive
    /// is rejected rather than being silently treated as complete.  Callers
    /// with a resolved configuration should use [`Self::new`] and provide the
    /// prepared bytes and source map explicitly.
    pub fn identity(
        source_id: ProjectSourceId,
        bytes: impl AsRef<[u8]>,
        configuration_id: impl Into<String>,
    ) -> Result<Self, PreparedSourceError> {
        let bytes_ref = bytes.as_ref();
        let tree = parse_clean(bytes_ref)?;
        if let Some((range, node_kind)) = first_preprocessor_node(tree.root_node(), bytes_ref) {
            return Err(PreparedSourceError::UnresolvedPreprocessor { range, node_kind });
        }
        let snapshot = SourceSnapshot::new(source_id.clone(), bytes.as_ref());
        let source_map = SourceMap::identity(snapshot)?;
        Self::new(
            source_id,
            bytes,
            source_map,
            configuration_id,
            PreparationFidelity::Complete,
            PreparationProvenance::Raw,
        )
    }

    /// Stable identity of the prepared byte snapshot.
    pub fn source_id(&self) -> &ProjectSourceId {
        &self.source_id
    }

    /// Borrow the exact parsed prepared bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrow the parse-clean tree built from [`Self::bytes`].
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Borrow the validated prepared-to-original source map.
    pub fn source_map(&self) -> &SourceMap {
        &self.source_map
    }

    /// Borrow the original source snapshots retained by the map.
    pub fn original_sources(&self) -> &[SourceSnapshot] {
        self.source_map.original_sources()
    }

    /// Configuration identity supplied by the project model.
    pub fn configuration_id(&self) -> &str {
        &self.configuration_id
    }

    /// Explicit completeness assertion retained with this source.
    pub fn fidelity(&self) -> PreparationFidelity {
        self.fidelity
    }

    /// Preparation provenance retained with this source.
    pub fn provenance(&self) -> PreparationProvenance {
        self.provenance
    }

    /// Map a prepared range through the retained source map.
    pub fn map_range(
        &self,
        prepared_range: Range<usize>,
    ) -> Result<Vec<MappedSourceSpan>, SourceMapError> {
        self.source_map.map_range(prepared_range)
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ProjectSourceId,
        Tree,
        Arc<[u8]>,
        SourceMap,
        String,
        PreparationFidelity,
        PreparationProvenance,
    ) {
        (
            self.source_id,
            self.tree,
            self.bytes,
            self.source_map,
            self.configuration_id,
            self.fidelity,
            self.provenance,
        )
    }
}

fn parse_clean(bytes: &[u8]) -> Result<Tree, PreparedSourceError> {
    let mut parser = Parser::new();
    parser
        .set_language(&crate::LANGUAGE.into())
        .map_err(|_| PreparedSourceError::ParserReturnedNoTree)?;
    let tree = parser
        .parse(bytes, None)
        .ok_or(PreparedSourceError::ParserReturnedNoTree)?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(PreparedSourceError::ParserErrors {
            range: root.start_byte()..root.end_byte(),
        });
    }
    Ok(tree)
}

fn first_preprocessor_node(
    root: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<(Range<usize>, String)> {
    if is_preprocessor_node(root, source) {
        return Some((root.start_byte()..root.end_byte(), root.kind().to_string()));
    }

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(found) = first_preprocessor_node(child, source) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn is_preprocessor_node(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    is_preprocessor_kind(node.kind())
        || (node.kind() == "comment"
            && source
                .get(node.byte_range())
                .is_some_and(|bytes| bytes.starts_with(b"(*$")))
}

pub(crate) fn is_preprocessor_kind(kind: &str) -> bool {
    kind.starts_with("pp")
}
