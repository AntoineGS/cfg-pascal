use std::{collections::HashMap, ops::Range};

use tree_sitter::Node;

/// Stable identity for a class type in one parsed source file.
///
/// The identity is intentionally local to [`ExceptionTypeIndex`].  It is used
/// only while building one file's CFGs and is never exposed through the public
/// API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TypeId(usize);

/// The semantic fact carried by an exceptional transfer.
///
/// `Known` is an exact class produced by a proven constructor. `SubtypeOf` is
/// a conservative bound used when re-raising from a typed handler: the
/// original exception is that class or one of its descendants, but the exact
/// runtime class is not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ExceptionTypeFact {
    Known(TypeId),
    SubtypeOf(TypeId),
    Unknown,
}

/// Result of comparing a raised class with one typed handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeMatch {
    Yes,
    No,
    Unknown,
}

/// A private, owned index of the type information that is safe to use for
/// exception dispatch in one source file.
///
/// This deliberately does not try to model the Pascal type system in full.
/// Only complete, non-generic local classes and transparent aliases are
/// resolved.  The index is same-file only: missing, imported, shadowed,
/// conditional, malformed, generic, `with`-implicit, and otherwise
/// unsupported information stays unknown so CFG edges are never removed on
/// an unproven assumption. Every modern `pp*` node is treated as an
/// unresolved file-wide barrier because the grammar may expose directives,
/// conditional blocks, or preprocessor fragments as siblings rather than as
/// ancestors of the declarations they affect; ordinary comments are not
/// barriers.
#[derive(Debug)]
pub(crate) struct ExceptionTypeIndex {
    module_name: Option<String>,
    root_scope: LexicalScopeId,
    scopes: Vec<LexicalScope>,
    types: Vec<TypeDeclaration>,
    unsupported_ranges: Vec<Range<usize>>,
    /// A `with` body has implicit member bindings that are not represented by
    /// the lexical tree.  Unqualified names in it cannot safely fall through
    /// to the file's global type namespace.
    with_ranges: Vec<Range<usize>>,
    /// The grammar exposes preprocessor directives as extra sibling nodes,
    /// rather than wrapping the declarations they affect.  A range check is
    /// therefore not sufficient to decide whether a type relationship is
    /// active; seeing an unresolved directive makes the whole file
    /// conservative.
    has_preprocessor_barrier: bool,
    pending_method_owners: Vec<PendingMethodOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LexicalScopeId(usize);

#[derive(Debug)]
struct LexicalScope {
    parent: Option<LexicalScopeId>,
    start: usize,
    end: usize,
    bindings: Vec<Binding>,
    /// The scope belongs to a method whose syntactic owner could not be
    /// resolved. Its missing parent must not be mistaken for the file root.
    unresolved_owner: bool,
}

#[derive(Debug)]
struct Binding {
    name: String,
    start: usize,
    kind: BindingKind,
}

#[derive(Debug, Clone, Copy)]
enum BindingKind {
    Type(TypeId),
    Value,
    Unsupported,
}

#[derive(Debug)]
struct TypeDeclaration {
    definition: TypeDefinition,
}

#[derive(Debug)]
enum TypeDefinition {
    Class {
        class_scope: LexicalScopeId,
        parent: ParentType,
        has_create_constructor: bool,
        has_create_member: bool,
        incomplete: bool,
    },
    Alias {
        target: Option<TypeReference>,
    },
    Unsupported,
}

#[derive(Debug)]
enum ParentType {
    None,
    Reference(TypeReference),
    Unsupported,
}

#[derive(Debug)]
struct TypeReference {
    parts: Vec<String>,
    scope: LexicalScopeId,
    offset: usize,
}

#[derive(Debug)]
struct PendingMethodOwner {
    routine_scope: LexicalScopeId,
    /// `None` records a syntactically present but unsupported owner, such as
    /// a generic instantiation.  It is important not to leave the routine
    /// attached to its lexical root in that case: doing so would make an
    /// unqualified member name fall through to an unrelated global type.
    owner_parts: Option<Vec<String>>,
    enclosing_scope: LexicalScopeId,
    offset: usize,
}

#[derive(Debug, Clone, Copy)]
enum Lookup {
    Type(TypeId),
    Value,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Knowledge {
    Yes,
    No,
    Unknown,
}

impl ExceptionTypeIndex {
    pub(crate) fn build(root: Node, source: &[u8]) -> Self {
        let mut index = Self {
            module_name: extract_module_name(root, source),
            root_scope: LexicalScopeId(0),
            scopes: Vec::new(),
            types: Vec::new(),
            unsupported_ranges: Vec::new(),
            with_ranges: Vec::new(),
            has_preprocessor_barrier: contains_preprocessor_directive(root),
            pending_method_owners: Vec::new(),
        };

        index.root_scope = index.new_scope(None, 0, source.len());
        index.collect_node(root, index.root_scope, source, false);
        index.resolve_pending_method_owners(source);
        index
    }

    /// Resolve the type produced by a syntactic `raise TException.Create`
    /// expression.  The returned fact is authoritative only when it is
    /// `Known`; all other forms remain conservative.
    pub(crate) fn raised_fact(&self, raise: Node, source: &[u8]) -> ExceptionTypeFact {
        let Some(parts) = constructor_parts(raise, source) else {
            return ExceptionTypeFact::Unknown;
        };
        if self.has_preprocessor_barrier || self.is_unsupported_at(raise.start_byte()) {
            return ExceptionTypeFact::Unknown;
        }
        if self.is_with_implicit_reference(&parts, raise.start_byte()) {
            return ExceptionTypeFact::Unknown;
        }

        let scope = self.scope_at(raise.start_byte());
        let Some(type_id) = self.resolve_parts(&parts, scope, raise.start_byte()) else {
            return ExceptionTypeFact::Unknown;
        };

        match self.constructor_knowledge(type_id) {
            Knowledge::Yes => ExceptionTypeFact::Known(type_id),
            Knowledge::No | Knowledge::Unknown => ExceptionTypeFact::Unknown,
        }
    }

    /// Resolve the type named by a typed `except` handler.
    pub(crate) fn handler_fact(&self, handler: Node, source: &[u8]) -> ExceptionTypeFact {
        let Some(exception) = handler.child_by_field_name("exception") else {
            return ExceptionTypeFact::Unknown;
        };
        if self.has_preprocessor_barrier || self.is_unsupported_at(exception.start_byte()) {
            return ExceptionTypeFact::Unknown;
        }
        let Some(parts) = type_reference_parts(exception, source) else {
            return ExceptionTypeFact::Unknown;
        };
        if self.is_with_implicit_reference(&parts, handler.start_byte()) {
            return ExceptionTypeFact::Unknown;
        }
        let scope = self.scope_at(handler.start_byte());
        let resolved = self.resolve_parts(&parts, scope, exception.start_byte());
        resolved
            .map(ExceptionTypeFact::Known)
            .unwrap_or(ExceptionTypeFact::Unknown)
    }

    pub(crate) fn match_handler(
        &self,
        raised: ExceptionTypeFact,
        handler: ExceptionTypeFact,
    ) -> TypeMatch {
        match (raised, handler) {
            (ExceptionTypeFact::Known(raised), ExceptionTypeFact::Known(handler)) => {
                self.is_subtype(raised, handler)
            }
            (ExceptionTypeFact::SubtypeOf(bound), ExceptionTypeFact::Known(handler)) => {
                self.match_subtype_bound(bound, handler)
            }
            _ => TypeMatch::Unknown,
        }
    }

    /// Compute the fact available to a typed handler body. An exact fact is
    /// retained only when every incoming path that can reach the handler is
    /// the same proven class. Otherwise the handler declaration provides the
    /// safe subtype bound.
    pub(crate) fn handler_context(
        &self,
        handler: ExceptionTypeFact,
        incoming: &[ExceptionTypeFact],
    ) -> ExceptionTypeFact {
        let ExceptionTypeFact::Known(handler_type) = handler else {
            return ExceptionTypeFact::Unknown;
        };

        let mut exact = None;
        for &raised in incoming {
            if matches!(self.match_handler(raised, handler), TypeMatch::No) {
                continue;
            }
            let ExceptionTypeFact::Known(raised_type) = raised else {
                return ExceptionTypeFact::SubtypeOf(handler_type);
            };
            if let Some(previous) = exact {
                if previous != raised_type {
                    return ExceptionTypeFact::SubtypeOf(handler_type);
                }
            } else {
                exact = Some(raised_type);
            }
        }

        exact
            .map(ExceptionTypeFact::Known)
            .unwrap_or(ExceptionTypeFact::SubtypeOf(handler_type))
    }

    fn collect_node(
        &mut self,
        node: Node,
        scope: LexicalScopeId,
        source: &[u8],
        conditional: bool,
    ) {
        let conditional = conditional || is_preprocessor_kind(node.kind());
        if is_preprocessor_kind(node.kind()) {
            self.has_preprocessor_barrier = true;
            self.unsupported_ranges
                .push(node.start_byte()..node.end_byte());
        }
        if node.kind() == "with" {
            self.with_ranges.push(node.start_byte()..node.end_byte());
        }

        match node.kind() {
            "defProc" => self.collect_routine(node, scope, source, conditional),
            "declTypes" => self.collect_type_section(node, scope, source, conditional),
            "declVars" | "declConsts" => self.collect_value_section(node, scope, source),
            "varDef" | "varAssignDef" => self.collect_inline_value_binding(node, scope, source),
            "declProc" => self.collect_proc_binding(node, scope, source, conditional),
            "exceptionHandler" => self.collect_exception_handler(node, scope, source, conditional),
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.collect_node(child, scope, source, conditional);
                }
            }
        }
    }

    fn collect_type_section(
        &mut self,
        node: Node,
        scope: LexicalScopeId,
        source: &[u8],
        conditional: bool,
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "declType" {
                self.collect_type_declaration(child, scope, source, conditional);
            }
        }
    }

    fn collect_value_section(&mut self, node: Node, scope: LexicalScopeId, source: &[u8]) {
        let mut cursor = node.walk();
        for declaration in node.named_children(&mut cursor) {
            if !matches!(declaration.kind(), "declVar" | "declConst") {
                continue;
            }
            for name in field_named_children(declaration, "name") {
                self.add_binding(
                    scope,
                    canonical(node_text(name, source)),
                    name.start_byte(),
                    BindingKind::Value,
                );
            }
        }
    }

    fn collect_inline_value_binding(&mut self, node: Node, scope: LexicalScopeId, source: &[u8]) {
        let Some(name) = direct_named_child(node, "identifier") else {
            return;
        };
        self.add_binding(
            scope,
            canonical(node_text(name, source)),
            name.start_byte(),
            BindingKind::Value,
        );
    }

    fn collect_type_declaration(
        &mut self,
        declaration: Node,
        scope: LexicalScopeId,
        source: &[u8],
        conditional: bool,
    ) {
        let Some(name_node) = declaration.child_by_field_name("name") else {
            return;
        };

        let type_id = TypeId(self.types.len());
        self.types.push(TypeDeclaration {
            definition: TypeDefinition::Unsupported,
        });

        let simple_name =
            (name_node.kind() == "identifier").then(|| canonical(node_text(name_node, source)));
        let binding_name = simple_name.clone().or_else(|| {
            first_identifier(name_node).map(|identifier| canonical(node_text(identifier, source)))
        });
        self.add_binding(
            scope,
            binding_name.unwrap_or_default(),
            declaration.start_byte(),
            if conditional || simple_name.is_none() {
                BindingKind::Unsupported
            } else {
                BindingKind::Type(type_id)
            },
        );

        if conditional || simple_name.is_none() || self.has_generic_syntax(declaration) {
            return;
        }

        let Some(type_node) = field_named_children(declaration, "type")
            .into_iter()
            .next()
            .and_then(unwrap_type_node)
        else {
            return;
        };

        let definition = match type_node.kind() {
            "declClass" if direct_named_child(type_node, "kClass").is_some() => {
                let class_scope =
                    self.new_scope(Some(scope), type_node.start_byte(), type_node.end_byte());
                let incomplete = direct_named_child(type_node, "kEnd").is_none();
                let parent = self.class_parent(type_node, scope, source);
                let has_create_constructor =
                    self.collect_class_members(type_node, class_scope, source);
                let has_create_member = self.scopes[class_scope.0]
                    .bindings
                    .iter()
                    .any(|binding| binding.name == "create");
                TypeDefinition::Class {
                    class_scope,
                    parent,
                    has_create_constructor,
                    has_create_member,
                    incomplete,
                }
            }
            "declClass" | "declIntf" | "declHelper" | "declMetaClass" => {
                TypeDefinition::Unsupported
            }
            "typeref" | "typerefDot" | "typerefPtr" => TypeDefinition::Alias {
                target: type_reference_parts(type_node, source).map(|parts| TypeReference {
                    parts,
                    scope,
                    offset: type_node.start_byte(),
                }),
            },
            _ => TypeDefinition::Unsupported,
        };

        self.types[type_id.0].definition = definition;
    }

    fn class_parent(&self, class_node: Node, scope: LexicalScopeId, source: &[u8]) -> ParentType {
        let parents = field_named_children(class_node, "parent");
        let Some(parent) = parents.into_iter().next() else {
            return ParentType::None;
        };
        if field_named_children(class_node, "parent").len() > 1 {
            return ParentType::Unsupported;
        }
        type_reference_parts(parent, source)
            .map(|parts| {
                ParentType::Reference(TypeReference {
                    parts,
                    scope,
                    offset: parent.start_byte(),
                })
            })
            .unwrap_or(ParentType::Unsupported)
    }

    fn collect_class_members(
        &mut self,
        class_node: Node,
        scope: LexicalScopeId,
        source: &[u8],
    ) -> bool {
        let mut has_create_constructor = false;
        let mut cursor = class_node.walk();
        for child in class_node.named_children(&mut cursor) {
            match child.kind() {
                "declTypes" => self.collect_type_section(child, scope, source, false),
                "declVars" | "declConsts" => self.collect_value_section(child, scope, source),
                "declField" | "declProp" => {
                    for name in field_named_children(child, "name") {
                        self.add_binding(
                            scope,
                            canonical(node_text(name, source)),
                            name.start_byte(),
                            BindingKind::Value,
                        );
                    }
                }
                "declProc" => {
                    if is_create_constructor(child, source) {
                        has_create_constructor = true;
                    }
                    self.collect_proc_binding(child, scope, source, false);
                }
                "declSection" => {
                    if self.collect_class_members(child, scope, source) {
                        has_create_constructor = true;
                    }
                }
                _ => continue,
            }
        }
        has_create_constructor
    }

    fn collect_proc_binding(
        &mut self,
        declaration: Node,
        scope: LexicalScopeId,
        source: &[u8],
        unsupported: bool,
    ) {
        let Some(name_node) = declaration.child_by_field_name("name") else {
            return;
        };
        let Some(parts) = name_parts(name_node, source) else {
            return;
        };
        if parts.len() == 1 {
            self.add_binding(
                scope,
                parts[0].clone(),
                declaration.start_byte(),
                if unsupported {
                    BindingKind::Unsupported
                } else {
                    BindingKind::Value
                },
            );
        }
    }

    fn collect_routine(
        &mut self,
        routine: Node,
        enclosing_scope: LexicalScopeId,
        source: &[u8],
        conditional: bool,
    ) {
        let Some(header) = routine
            .child_by_field_name("header")
            .or_else(|| direct_named_child(routine, "declProc"))
        else {
            return;
        };

        self.collect_proc_binding(header, enclosing_scope, source, conditional);

        let routine_scope = self.new_scope(
            Some(enclosing_scope),
            routine.start_byte(),
            routine.end_byte(),
        );
        if let Some(args) = field_named_children(header, "args").into_iter().next() {
            let mut cursor = args.walk();
            for argument in args.named_children(&mut cursor) {
                if argument.kind() != "declArg" {
                    continue;
                }
                for name in field_named_children(argument, "name") {
                    self.add_binding(
                        routine_scope,
                        canonical(node_text(name, source)),
                        name.start_byte(),
                        BindingKind::Value,
                    );
                }
            }
        }

        let Some(name_node) = header.child_by_field_name("name") else {
            return;
        };

        let owner_parts = name_parts(name_node, source)
            .and_then(|parts| (parts.len() > 1).then(|| parts[..parts.len() - 1].to_vec()));
        let has_method_owner = owner_parts.is_some()
            || (name_node.kind() == "genericDot" && name_parts(name_node, source).is_none());
        if has_method_owner {
            self.pending_method_owners.push(PendingMethodOwner {
                routine_scope,
                owner_parts: if owner_parts.is_some() {
                    owner_parts
                } else {
                    None
                },
                enclosing_scope,
                offset: routine.start_byte(),
            });
            self.add_binding(
                routine_scope,
                "self".to_string(),
                routine.start_byte(),
                BindingKind::Value,
            );
        }

        if direct_named_child(header, "kFunction").is_some() {
            self.add_binding(
                routine_scope,
                "result".to_string(),
                routine.start_byte(),
                BindingKind::Value,
            );
            if let Some(parts) = name_parts(name_node, source) {
                if let Some(result_name) = parts.last() {
                    self.add_binding(
                        routine_scope,
                        result_name.clone(),
                        routine.start_byte(),
                        BindingKind::Value,
                    );
                }
            }
        }

        for local in field_named_children(routine, "local") {
            self.collect_node(local, routine_scope, source, conditional);
        }

        if let Some(body) = routine.child_by_field_name("body") {
            self.collect_node(body, routine_scope, source, conditional);
        }
    }

    fn collect_exception_handler(
        &mut self,
        handler: Node,
        enclosing_scope: LexicalScopeId,
        source: &[u8],
        conditional: bool,
    ) {
        let Some(body) = handler.child_by_field_name("body") else {
            return;
        };
        let handler_scope =
            self.new_scope(Some(enclosing_scope), body.start_byte(), handler.end_byte());

        if let Some(variable) = handler.child_by_field_name("variable") {
            if variable.kind() == "identifier" {
                self.add_binding(
                    handler_scope,
                    canonical(node_text(variable, source)),
                    body.start_byte(),
                    if conditional {
                        BindingKind::Unsupported
                    } else {
                        BindingKind::Value
                    },
                );
            }
        }

        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            self.collect_node(child, handler_scope, source, conditional);
        }
    }

    fn resolve_pending_method_owners(&mut self, _source: &[u8]) {
        let pending = std::mem::take(&mut self.pending_method_owners);
        for owner in pending {
            let Some(owner_parts) = owner.owner_parts else {
                self.mark_unresolved_owner(owner.routine_scope);
                continue;
            };
            let Some(type_id) =
                self.resolve_parts(&owner_parts, owner.enclosing_scope, owner.offset)
            else {
                self.mark_unresolved_owner(owner.routine_scope);
                continue;
            };
            let Some(class_scope) = self.class_scope(type_id) else {
                self.mark_unresolved_owner(owner.routine_scope);
                continue;
            };
            self.scopes[owner.routine_scope.0].parent = Some(class_scope);
        }
    }

    fn mark_unresolved_owner(&mut self, routine_scope: LexicalScopeId) {
        let scope = &mut self.scopes[routine_scope.0];
        scope.parent = None;
        scope.unresolved_owner = true;
    }

    fn new_scope(
        &mut self,
        parent: Option<LexicalScopeId>,
        start: usize,
        end: usize,
    ) -> LexicalScopeId {
        let id = LexicalScopeId(self.scopes.len());
        self.scopes.push(LexicalScope {
            parent,
            start,
            end,
            bindings: Vec::new(),
            unresolved_owner: false,
        });
        id
    }

    fn add_binding(
        &mut self,
        scope: LexicalScopeId,
        name: String,
        start: usize,
        kind: BindingKind,
    ) {
        if name.is_empty() {
            return;
        }
        self.scopes[scope.0]
            .bindings
            .push(Binding { name, start, kind });
    }

    fn scope_at(&self, offset: usize) -> LexicalScopeId {
        let mut selected = self.root_scope;
        for (index, scope) in self.scopes.iter().enumerate() {
            if scope.start > offset || offset >= scope.end {
                continue;
            }
            let selected_scope = &self.scopes[selected.0];
            if scope.start > selected_scope.start
                || (scope.start == selected_scope.start && scope.end < selected_scope.end)
            {
                selected = LexicalScopeId(index);
            }
        }
        selected
    }

    fn resolve_parts(
        &self,
        parts: &[String],
        scope: LexicalScopeId,
        offset: usize,
    ) -> Option<TypeId> {
        let mut seen = HashMap::new();
        self.resolve_parts_with_seen(parts, scope, offset, &mut seen)
    }

    fn resolve_parts_with_seen(
        &self,
        parts: &[String],
        scope: LexicalScopeId,
        offset: usize,
        seen: &mut HashMap<TypeId, ()>,
    ) -> Option<TypeId> {
        if parts.is_empty() {
            return None;
        }
        let (scope, parts) = self.module_qualified_scope(parts, scope, offset)?;
        let first = match self.lookup(scope, &parts[0], offset) {
            Lookup::Type(type_id) => self.resolve_type_id(type_id, seen)?,
            Lookup::Value | Lookup::Unknown => return None,
        };
        let mut resolved = first;

        for part in &parts[1..] {
            let mut member_seen = HashMap::new();
            resolved = match self.lookup_class_member(resolved, part, offset, &mut member_seen)? {
                Lookup::Type(type_id) => self.resolve_type_id(type_id, seen)?,
                Lookup::Value | Lookup::Unknown => return None,
            };
        }

        Some(resolved)
    }

    /// Select the lookup scope for a possibly module-qualified name.
    ///
    /// An unresolved method owner leaves implicit members unmodeled, so a
    /// module-looking first component is ambiguous in that scope.
    fn module_qualified_scope<'a>(
        &self,
        parts: &'a [String],
        scope: LexicalScopeId,
        offset: usize,
    ) -> Option<(LexicalScopeId, &'a [String])> {
        let is_module_qualified = parts.len() > 1
            && self
                .module_name
                .as_deref()
                .is_some_and(|module| module == parts[0])
            && !self.qualifier_is_shadowed(scope, &parts[0], offset);
        if is_module_qualified {
            if self.has_unresolved_owner(scope) {
                return None;
            }
            Some((self.root_scope, &parts[1..]))
        } else {
            Some((scope, parts))
        }
    }

    fn has_unresolved_owner(&self, scope: LexicalScopeId) -> bool {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if self.scopes[scope.0].unresolved_owner {
                return true;
            }
            current = self.scopes[scope.0].parent;
        }
        false
    }

    fn qualifier_is_shadowed(&self, scope: LexicalScopeId, name: &str, offset: usize) -> bool {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if self.has_binding(scope, name, offset) {
                return true;
            }
            if let Some(type_id) = self.class_type_for_scope(scope) {
                let mut seen = HashMap::new();
                if self
                    .lookup_class_member(type_id, name, offset, &mut seen)
                    .is_some()
                {
                    return true;
                }
            }
            current = self.scopes[scope.0].parent;
        }
        false
    }

    fn lookup_class_member(
        &self,
        type_id: TypeId,
        name: &str,
        offset: usize,
        seen: &mut HashMap<TypeId, ()>,
    ) -> Option<Lookup> {
        if seen.insert(type_id, ()).is_some() {
            return Some(Lookup::Unknown);
        }

        let TypeDefinition::Class {
            class_scope,
            parent,
            incomplete,
            ..
        } = &self.types[type_id.0].definition
        else {
            seen.remove(&type_id);
            return Some(Lookup::Unknown);
        };

        if *incomplete {
            seen.remove(&type_id);
            return Some(Lookup::Unknown);
        }

        if self.has_binding(*class_scope, name, offset) {
            let result = self.lookup_direct_before(*class_scope, name, offset);
            seen.remove(&type_id);
            return Some(result);
        }

        let result = match parent {
            ParentType::None => None,
            ParentType::Unsupported => Some(Lookup::Unknown),
            ParentType::Reference(parent) => {
                let Some(parent_id) =
                    self.resolve_parts_with_seen(&parent.parts, parent.scope, parent.offset, seen)
                else {
                    seen.remove(&type_id);
                    return Some(Lookup::Unknown);
                };
                self.lookup_class_member(parent_id, name, offset, seen)
            }
        };
        seen.remove(&type_id);
        result
    }

    fn class_type_for_scope(&self, scope: LexicalScopeId) -> Option<TypeId> {
        self.types
            .iter()
            .enumerate()
            .find_map(|(index, declaration)| {
                let TypeDefinition::Class { class_scope, .. } = &declaration.definition else {
                    return None;
                };
                (*class_scope == scope).then_some(TypeId(index))
            })
    }

    fn resolve_type_id(&self, type_id: TypeId, seen: &mut HashMap<TypeId, ()>) -> Option<TypeId> {
        if seen.insert(type_id, ()).is_some() {
            return None;
        }

        let resolved = match &self.types[type_id.0].definition {
            TypeDefinition::Class {
                incomplete: true, ..
            } => None,
            TypeDefinition::Class {
                incomplete: false, ..
            } => Some(type_id),
            TypeDefinition::Alias {
                target: Some(target),
            } => self.resolve_parts_with_seen(&target.parts, target.scope, target.offset, seen),
            TypeDefinition::Alias { target: None } | TypeDefinition::Unsupported => None,
        };
        seen.remove(&type_id);
        resolved
    }

    fn constructor_knowledge(&self, type_id: TypeId) -> Knowledge {
        let mut seen = HashMap::new();
        self.constructor_knowledge_with_seen(type_id, &mut seen)
    }

    fn constructor_knowledge_with_seen(
        &self,
        type_id: TypeId,
        seen: &mut HashMap<TypeId, ()>,
    ) -> Knowledge {
        if seen.insert(type_id, ()).is_some() {
            return Knowledge::Unknown;
        }

        let result = match &self.types[type_id.0].definition {
            TypeDefinition::Class {
                incomplete: true, ..
            } => Knowledge::Unknown,
            TypeDefinition::Class {
                has_create_constructor: true,
                ..
            } => Knowledge::Yes,
            TypeDefinition::Class {
                has_create_member: true,
                ..
            } => Knowledge::Unknown,
            TypeDefinition::Class {
                parent: ParentType::None,
                ..
            } => Knowledge::No,
            TypeDefinition::Class {
                parent: ParentType::Unsupported,
                ..
            } => Knowledge::Unknown,
            TypeDefinition::Class {
                parent: ParentType::Reference(parent),
                ..
            } => {
                let Some(parent_id) =
                    self.resolve_parts_with_seen(&parent.parts, parent.scope, parent.offset, seen)
                else {
                    seen.remove(&type_id);
                    return Knowledge::Unknown;
                };
                self.constructor_knowledge_with_seen(parent_id, seen)
            }
            TypeDefinition::Alias { .. } | TypeDefinition::Unsupported => Knowledge::Unknown,
        };

        seen.remove(&type_id);
        result
    }

    fn is_subtype(&self, raised: TypeId, handler: TypeId) -> TypeMatch {
        let mut current = raised;
        let mut seen = HashMap::new();

        loop {
            if current == handler {
                return TypeMatch::Yes;
            }
            if seen.insert(current, ()).is_some() {
                return TypeMatch::Unknown;
            }

            let TypeDefinition::Class {
                parent, incomplete, ..
            } = &self.types[current.0].definition
            else {
                return TypeMatch::Unknown;
            };
            if *incomplete {
                return TypeMatch::Unknown;
            }
            let ParentType::Reference(parent) = parent else {
                return match parent {
                    ParentType::None => TypeMatch::No,
                    ParentType::Unsupported => TypeMatch::Unknown,
                    ParentType::Reference(_) => unreachable!(),
                };
            };

            let Some(parent_id) =
                self.resolve_parts_with_seen(&parent.parts, parent.scope, parent.offset, &mut seen)
            else {
                return TypeMatch::Unknown;
            };
            current = parent_id;
        }
    }

    fn match_subtype_bound(&self, bound: TypeId, handler: TypeId) -> TypeMatch {
        match self.is_subtype(bound, handler) {
            TypeMatch::Yes => TypeMatch::Yes,
            TypeMatch::No => match self.is_subtype(handler, bound) {
                TypeMatch::Yes => TypeMatch::Unknown,
                TypeMatch::No => TypeMatch::No,
                TypeMatch::Unknown => TypeMatch::Unknown,
            },
            TypeMatch::Unknown => TypeMatch::Unknown,
        }
    }

    fn class_scope(&self, type_id: TypeId) -> Option<LexicalScopeId> {
        match &self.types[type_id.0].definition {
            TypeDefinition::Class {
                class_scope,
                incomplete: false,
                ..
            } => Some(*class_scope),
            TypeDefinition::Alias { .. } | TypeDefinition::Unsupported => None,
            TypeDefinition::Class {
                incomplete: true, ..
            } => None,
        }
    }

    fn lookup(&self, scope: LexicalScopeId, name: &str, offset: usize) -> Lookup {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if let Some(type_id) = self.class_type_for_scope(scope) {
                let mut seen = HashMap::new();
                if let Some(result) = self.lookup_class_member(type_id, name, offset, &mut seen) {
                    return result;
                }
            } else if self.has_binding(scope, name, offset) {
                return self.lookup_direct_before(scope, name, offset);
            }
            current = self.scopes[scope.0].parent;
        }
        Lookup::Unknown
    }

    fn lookup_direct_before(&self, scope: LexicalScopeId, name: &str, offset: usize) -> Lookup {
        let bindings: Vec<&Binding> = self.scopes[scope.0]
            .bindings
            .iter()
            .filter(|binding| binding.name == name && binding.start <= offset)
            .collect();
        let Some(latest_start) = bindings.iter().map(|binding| binding.start).max() else {
            return Lookup::Unknown;
        };
        let latest: Vec<&Binding> = bindings
            .into_iter()
            .filter(|binding| binding.start == latest_start)
            .collect();
        if latest.len() != 1 {
            return Lookup::Unknown;
        }
        match latest[0].kind {
            BindingKind::Type(type_id) => Lookup::Type(type_id),
            BindingKind::Value | BindingKind::Unsupported => Lookup::Value,
        }
    }

    fn has_binding(&self, scope: LexicalScopeId, name: &str, offset: usize) -> bool {
        self.scopes[scope.0]
            .bindings
            .iter()
            .any(|binding| binding.name == name && binding.start <= offset)
    }

    fn is_unsupported_at(&self, offset: usize) -> bool {
        self.unsupported_ranges
            .iter()
            .any(|range| range.start <= offset && offset < range.end)
    }

    /// Dotted names can still begin with an implicit member of a `with`
    /// receiver, so their length does not establish explicit qualification.
    fn is_with_implicit_reference(&self, parts: &[String], offset: usize) -> bool {
        !parts.is_empty()
            && self
                .with_ranges
                .iter()
                .any(|range| range.start <= offset && offset < range.end)
    }

    fn has_generic_syntax(&self, node: Node) -> bool {
        if matches!(node.kind(), "genericTpl" | "typerefTpl" | "kGeneric")
            || is_preprocessor_kind(node.kind())
        {
            return true;
        }
        let mut cursor = node.walk();
        let result = node
            .named_children(&mut cursor)
            .any(|child| self.has_generic_syntax(child));
        result
    }
}

fn extract_module_name(root: Node, source: &[u8]) -> Option<String> {
    let module = direct_named_child_any(root, &["unit", "program", "library"])?;
    let module_name = direct_named_child_any(module, &["moduleName"])?;
    Some(canonical(node_text(module_name, source)))
}

fn contains_preprocessor_directive(node: Node) -> bool {
    if is_preprocessor_kind(node.kind()) {
        return true;
    }
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(contains_preprocessor_directive);
    result
}

fn is_preprocessor_kind(kind: &str) -> bool {
    kind.starts_with("pp")
}

fn constructor_parts(node: Node, source: &[u8]) -> Option<Vec<String>> {
    let exception = node.child_by_field_name("exception")?;
    match exception.kind() {
        "exprCall" => constructor_call_parts(exception, source),
        "exprDot" => constructor_parts_from_entity(exception, source),
        "exprParens" => {
            let mut cursor = exception.walk();
            let parts =
                exception
                    .named_children(&mut cursor)
                    .find_map(|child| match child.kind() {
                        "exprCall" => constructor_call_parts(child, source),
                        "exprDot" => constructor_parts_from_entity(child, source),
                        _ => None,
                    });
            parts
        }
        _ => None,
    }
}

fn constructor_call_parts(node: Node, source: &[u8]) -> Option<Vec<String>> {
    let entity = node.child_by_field_name("entity")?;
    constructor_parts_from_entity(entity, source)
}

fn constructor_parts_from_entity(node: Node, source: &[u8]) -> Option<Vec<String>> {
    let mut parts = name_parts(node, source)?;
    let constructor = parts.pop()?;
    constructor.eq_ignore_ascii_case("create").then_some(parts)
}

fn type_reference_parts(node: Node, source: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "typeref" | "type" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            if children.len() != 1 {
                return None;
            }
            type_reference_parts(children[0], source)
        }
        "typerefDot" => {
            let lhs = node.child_by_field_name("lhs")?;
            let rhs = node.child_by_field_name("rhs")?;
            let mut parts = type_reference_parts(lhs, source)?;
            parts.extend(type_reference_parts(rhs, source)?);
            Some(parts)
        }
        "typerefPtr" | "typerefTpl" | "genericDot" | "genericTpl" => None,
        "identifier" => Some(vec![canonical(node_text(node, source))]),
        _ => None,
    }
}

fn name_parts(node: Node, source: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![canonical(node_text(node, source))]),
        "exprDot" | "genericDot" => {
            let lhs = node.child_by_field_name("lhs")?;
            let rhs = node.child_by_field_name("rhs")?;
            let mut parts = name_parts(lhs, source)?;
            parts.extend(name_parts(rhs, source)?);
            Some(parts)
        }
        "genericTpl" => None,
        _ => None,
    }
}

fn is_create_constructor(node: Node, source: &[u8]) -> bool {
    direct_named_child(node, "kConstructor").is_some()
        && node
            .child_by_field_name("name")
            .and_then(|name| name_parts(name, source))
            .is_some_and(|parts| parts.len() == 1 && parts[0] == "create")
}

fn unwrap_type_node<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() != "type" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    (children.len() == 1).then_some(children[0])
}

fn field_named_children<'tree>(node: Node<'tree>, field: &str) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor)
        .filter(|child| child.is_named())
        .collect()
}

fn direct_named_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    direct_named_child_any(node, &[kind])
}

fn direct_named_child_any<'tree>(node: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let result = node
        .named_children(&mut cursor)
        .find(|child| kinds.contains(&child.kind()));
    result
}

fn first_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let result = node.named_children(&mut cursor).find_map(first_identifier);
    result
}

fn node_text(node: Node, source: &[u8]) -> String {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

fn canonical(name: String) -> String {
    name.to_ascii_lowercase()
}
