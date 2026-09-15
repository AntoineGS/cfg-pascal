use cfg_core::{BlockId, Cfg, EdgeKind};
use cfg_pascal::build_file_cfgs;
use tree_sitter::{Parser, Tree};

fn parse_clean(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&cfg_pascal::LANGUAGE.into())
        .expect("failed to set Pascal language");
    let tree = parser
        .parse(source.as_bytes(), None)
        .expect("parser returned no tree");
    assert!(
        !tree.root_node().has_error(),
        "semantic-review fixture must be parser-clean:\n{}",
        tree.root_node().to_sexp()
    );
    tree
}

fn cfg_for<'a>(cfgs: &'a [Cfg], name: &str) -> &'a Cfg {
    cfgs.iter()
        .find(|cfg| cfg.proc_name == name)
        .unwrap_or_else(|| panic!("CFG for {name:?} not found"))
}

fn block_with_stmt(cfg: &Cfg, source: &str, kind: &str, text: &str) -> BlockId {
    let blocks: Vec<_> = cfg
        .graph
        .node_indices()
        .filter_map(|index| {
            let block = &cfg.graph[index];
            block
                .stmts
                .iter()
                .any(|stmt| {
                    stmt.node_kind == kind
                        && source
                            .get(stmt.byte_range.clone())
                            .is_some_and(|stmt_text| stmt_text.contains(text))
                })
                .then(|| BlockId::from(index))
        })
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "expected one {kind:?} statement containing {text:?}, got {blocks:?}"
    );
    blocks[0]
}

fn successors(cfg: &Cfg, from: BlockId) -> Vec<(BlockId, EdgeKind)> {
    cfg.graph
        .edge_indices()
        .filter_map(|edge| {
            let (source, target) = cfg.graph.edge_endpoints(edge)?;
            (source == from.index()).then(|| (BlockId::from(target), cfg.graph[edge].clone()))
        })
        .collect()
}

fn successful_raise_block(cfg: &Cfg, raise: BlockId) -> BlockId {
    successors(cfg, raise)
        .into_iter()
        .find_map(|(target, kind)| {
            (kind == EdgeKind::Normal && cfg.graph[target.index()].stmts.is_empty())
                .then_some(target)
        })
        .expect("constructor raise must have a synthetic successful-raise block")
}

fn assert_successful_raise_reaches(source: &str, procedure: &str, raise_text: &str, handler: &str) {
    let tree = parse_clean(source);
    let cfgs = build_file_cfgs(&tree, source.as_bytes());
    let cfg = cfg_for(&cfgs, procedure);
    let raise = block_with_stmt(cfg, source, "raise", raise_text);
    let successful_raise = successful_raise_block(cfg, raise);
    let handler = block_with_stmt(cfg, source, "statement", handler);

    assert!(
        successors(cfg, successful_raise).contains(&(handler, EdgeKind::ExceptionThrow)),
        "a successful or conservatively unresolved raise must retain {handler:?}"
    );
}

#[test]
fn same_module_qualification_uses_module_scope_not_a_local_shadow() {
    assert_successful_raise_reaches(
        r#"
unit U;
interface
implementation
type
  E = class constructor Create; end;
  RootE = E;
procedure P;
type
  E = class constructor Create; end;
begin
  try
    raise U.E.Create;
  except
    on E do WrongLocal;
    on RootE do CorrectRoot;
  end;
end;
end.
"#,
        "P",
        "raise U.E.Create",
        "CorrectRoot",
    );
}

#[test]
fn a_value_shadowing_the_module_qualifier_blocks_module_fallback() {
    assert_successful_raise_reaches(
        r#"
unit U;
interface
implementation
type
  E = class constructor Create; end;
  RootE = E;
procedure P(U: Integer);
begin
  try
    raise U.E.Create;
  except
    on E do First;
    on RootE do Second;
  end;
end;
end.
"#,
        "P",
        "raise U.E.Create",
        "First",
    );
}

#[test]
fn ordinary_comments_do_not_disable_precise_same_file_dispatch() {
    let source = r#"
unit CommentBarrier;
interface
implementation
type
  A = class constructor Create; end;
  B = class constructor Create; end;
procedure P;
begin
  try
    { this comment is not a preprocessor directive }
    raise A.Create;
  except
    on B do Wrong;
    on A do Right;
  end;
end;
end.
"#;
    let tree = parse_clean(source);
    let cfgs = build_file_cfgs(&tree, source.as_bytes());
    let cfg = cfg_for(&cfgs, "P");
    let raise = block_with_stmt(cfg, source, "raise", "raise A.Create");
    let successful_raise = successful_raise_block(cfg, raise);
    let right = block_with_stmt(cfg, source, "statement", "Right");

    assert_eq!(
        successors(cfg, successful_raise),
        vec![(right, EdgeKind::ExceptionThrow)],
        "ordinary comments must not turn a proven constructor into an unknown raise"
    );
}

#[test]
fn inherited_member_shadowing_does_not_fall_back_to_a_global_type() {
    assert_successful_raise_reaches(
        r#"
unit U;
interface
implementation
type
  E = class constructor Create; end;
  Other = class(E) constructor Create; end;
  Meta = class of E;
  Parent = class E: Meta; end;
  Child = class(Parent) procedure P; end;
procedure Child.P;
begin
  E := Other;
  try
    raise E.Create;
  except
    on Other do First;
    on U.E do Second;
  end;
end;
end.
"#,
        "Child.P",
        "raise E.Create",
        "First",
    );
}

#[test]
fn with_bodies_do_not_resolve_unqualified_receivers_as_globals() {
    assert_successful_raise_reaches(
        r#"
unit U;
interface
implementation
type
  E = class constructor Create; end;
  Other = class(E) constructor Create; end;
  Meta = class of E;
  Holder = class E: Meta; end;
procedure P(H: Holder);
begin
  H.E := Other;
  with H do
  try
    raise E.Create;
  except
    on Other do First;
    on U.E do Second;
  end;
end;
end.
"#,
        "P",
        "raise E.Create",
        "First",
    );
}

#[test]
fn preprocessor_directives_make_conditional_aliases_conservative() {
    assert_successful_raise_reaches(
        r#"
unit ConditionalAlias;
interface
implementation
type
  A = class constructor Create; end;
  B = class constructor Create; end;
{$IFDEF CHOOSE_A}
  E = A;
{$ELSE}
  E = B;
{$ENDIF}
procedure P;
begin
  try
    raise E.Create;
  except
    on A do First;
    on B do Second;
  end;
end;
end.
"#,
        "P",
        "raise E.Create",
        "First",
    );
}

#[test]
fn implicit_function_values_shadow_same_named_global_types() {
    assert_successful_raise_reaches(
        r#"
unit ImplicitResult;
interface
implementation
type
  Result = class constructor Create; end;
  Child = class(Result) constructor Create; end;
  RootResult = Result;
  Meta = class of RootResult;
function P: Meta;
begin
  Result := Child;
  try
    raise Result.Create;
  except
    on Child do First;
    on RootResult do Second;
  end;
end;
end.
"#,
        "P",
        "raise Result.Create",
        "First",
    );
}

#[test]
fn unresolved_generic_method_owners_block_global_fallbacks() {
    assert_successful_raise_reaches(
        r#"
unit GenericOwner;
interface
implementation
type
  E = class constructor Create; end;
  Other = class(E) constructor Create; end;
  Meta = class of E;
  Holder<T> = class E: Meta; procedure P; end;
procedure Holder<T>.P;
begin
  E := Other;
  try
    raise E.Create;
  except
    on Other do First;
    on U.E do Second;
  end;
end;
end.
"#,
        "P",
        "raise E.Create",
        "First",
    );
}

#[test]
fn conditional_handler_reraises_keep_all_outer_handler_alternatives() {
    let source = r#"
unit ConditionalReraise;
interface
implementation
type
  A = class constructor Create; end;
  B = class constructor Create; end;
{$IFDEF CHOOSE_A}
  E = A;
{$ELSE}
  E = B;
{$ENDIF}
procedure P;
begin
  try
    try
      raise UnknownError;
    except
      on E do raise;
  end;
  except
    on A do First;
    on B do Second;
  end;
end;
end.
"#;
    let tree = parse_clean(source);
    let cfgs = build_file_cfgs(&tree, source.as_bytes());
    let cfg = cfg_for(&cfgs, "P");
    let reraise = block_with_stmt(cfg, source, "raise", "raise;");
    let first = block_with_stmt(cfg, source, "statement", "First");

    assert!(
        successors(cfg, reraise).contains(&(first, EdgeKind::ExceptionThrow)),
        "a conditional typed handler must not make a bare re-raise look like only B"
    );
}
