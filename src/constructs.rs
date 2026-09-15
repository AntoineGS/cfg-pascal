use cfg_core::BlockId;
use tree_sitter::Node;

/// Extract the UTF-8 text of a node from the source bytes.
pub(crate) fn node_text(node: Node, source: &[u8]) -> String {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

/// Check whether a node represents a call to `Exit`.
///
/// In tree-sitter-pascal, a standalone `Exit;` is parsed as a `statement`
/// node containing an `identifier` child with text "Exit" (case-insensitive).
/// It can also appear as a bare `identifier` child of a `block`.
pub(crate) fn is_exit_call(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "statement" => {
            // A statement wraps the expression for both `Exit;` and
            // `Exit(value);`. Delegate to the expression classifier so the
            // latter is not mistaken for an ordinary fallthrough statement.
            let mut cursor = node.walk();
            let is_exit = node
                .children(&mut cursor)
                .any(|child| is_exit_call(child, source));
            is_exit
        }
        "identifier" => {
            let text = node_text(node, source);
            text.eq_ignore_ascii_case("exit")
        }
        "exprCall" => {
            // Exit(...) with a return value
            if let Some(entity) = node.child_by_field_name("entity") {
                if entity.kind() == "identifier" {
                    let text = node_text(entity, source);
                    return text.eq_ignore_ascii_case("exit");
                }
            }
            false
        }
        _ => false,
    }
}

/// Check whether a node represents a call to `Break`.
pub(crate) fn is_break_call(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let text = node_text(child, source);
                    if text.eq_ignore_ascii_case("break") {
                        return true;
                    }
                }
            }
            false
        }
        "identifier" => {
            let text = node_text(node, source);
            text.eq_ignore_ascii_case("break")
        }
        _ => false,
    }
}

/// Check whether a node represents a call to `Continue`.
pub(crate) fn is_continue_call(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let text = node_text(child, source);
                    if text.eq_ignore_ascii_case("continue") {
                        return true;
                    }
                }
            }
            false
        }
        "identifier" => {
            let text = node_text(node, source);
            text.eq_ignore_ascii_case("continue")
        }
        _ => false,
    }
}

/// Identifier for a cleanup scope that may need to be unwound by a control
/// transfer. The builder deliberately keeps this separate from block IDs:
/// a transfer target can be a block inside an enclosing `try..finally` even
/// when the transfer itself originated in a nested scope.
pub(crate) type ScopeId = usize;

/// The kind of abrupt completion produced by a statement or expression.
///
/// `Goto` will fit this model later by carrying a block target and the target
/// scope set, without changing the cleanup routing API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Exit,
    Break,
    Continue,
    Exception,
}

/// A pending control transfer whose edge has not yet been routed through all
/// enclosing cleanup scopes.
#[derive(Debug, Clone)]
pub(crate) struct PendingTransfer {
    pub source: BlockId,
    pub kind: TransferKind,
    pub target: Option<BlockId>,
    pub target_scopes: Vec<ScopeId>,
    pub from_finally: bool,
    pub exception_type: Option<String>,
}

impl PendingTransfer {
    pub(crate) fn exit(source: BlockId) -> Self {
        Self {
            source,
            kind: TransferKind::Exit,
            target: None,
            target_scopes: Vec::new(),
            from_finally: false,
            exception_type: None,
        }
    }

    pub(crate) fn block_target(
        source: BlockId,
        kind: TransferKind,
        target: BlockId,
        target_scopes: Vec<ScopeId>,
    ) -> Self {
        debug_assert!(matches!(kind, TransferKind::Break | TransferKind::Continue));
        Self {
            source,
            kind,
            target: Some(target),
            target_scopes,
            from_finally: false,
            exception_type: None,
        }
    }

    pub(crate) fn exception(source: BlockId) -> Self {
        Self {
            source,
            kind: TransferKind::Exception,
            target: None,
            target_scopes: Vec::new(),
            from_finally: false,
            exception_type: None,
        }
    }

    pub(crate) fn exception_with_type(source: BlockId, exception_type: String) -> Self {
        Self {
            exception_type: Some(exception_type),
            ..Self::exception(source)
        }
    }

    /// Whether this transfer's target lies outside `scope_id` and therefore
    /// must pass through that scope's finalizer.
    pub(crate) fn leaves_scope(&self, scope_id: ScopeId) -> bool {
        match self.kind {
            TransferKind::Exception | TransferKind::Exit => true,
            TransferKind::Break | TransferKind::Continue => !self.target_scopes.contains(&scope_id),
        }
    }

    pub(crate) fn with_source(&self, source: BlockId) -> Self {
        Self {
            source,
            kind: self.kind,
            target: self.target,
            target_scopes: self.target_scopes.clone(),
            from_finally: true,
            exception_type: self.exception_type.clone(),
        }
    }
}

/// Infer the concrete type from a simple `raise TException.Create(...)`
/// expression. More complex expressions remain unknown and therefore use the
/// conservative all-handler/unmatched dispatch.
pub(crate) fn raised_exception_type(node: Node, source: &[u8]) -> Option<String> {
    let exception = node.child_by_field_name("exception")?;
    leading_identifier(exception, source)
}

fn leading_identifier(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(node, source)),
        "exprCall" => node
            .child_by_field_name("entity")
            .and_then(|entity| leading_identifier(entity, source)),
        "exprDot" => node
            .child_by_field_name("lhs")
            .and_then(|lhs| leading_identifier(lhs, source)),
        "exprParens" => {
            let mut cursor = node.walk();
            let child = node
                .named_children(&mut cursor)
                .next()
                .and_then(|child| leading_identifier(child, source));
            child
        }
        _ => None,
    }
}

/// Context for loop constructs: tracks where `break` and `continue` jump to.
pub(crate) struct LoopFrame {
    pub continue_target: BlockId,
    pub break_target: BlockId,
    pub continue_scopes: Vec<ScopeId>,
    pub break_scopes: Vec<ScopeId>,
}

/// The result of walking one statement or statement sequence.
///
/// A plain `Option<BlockId>` cannot represent an `if` where one arm raises
/// and the other falls through, nor can it preserve an `Exit`/`Break`/`raise`
/// while a `finally` body is being built. Keeping abrupt paths alongside the
/// normal continuation makes those paths explicit without changing cfg-core's
/// public graph types.
#[derive(Debug, Default)]
pub(crate) struct Flow {
    pub normal: Option<BlockId>,
    pub transfers: Vec<PendingTransfer>,
}

impl Flow {
    pub(crate) fn normal(block: BlockId) -> Self {
        Self {
            normal: Some(block),
            transfers: Vec::new(),
        }
    }

    pub(crate) fn transfer(transfer: PendingTransfer) -> Self {
        Self {
            normal: None,
            transfers: vec![transfer],
        }
    }
}
