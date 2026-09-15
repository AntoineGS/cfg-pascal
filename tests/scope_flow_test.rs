use std::ops::Range;

use cfg_core::{BlockId, Cfg, EdgeKind};
use cfg_pascal::build_file_cfgs;
use tree_sitter::{Parser, Tree};

fn parse_clean(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    let language = tree_sitter_pascal::LANGUAGE;
    parser
        .set_language(&language.into())
        .expect("failed to set Pascal language");

    let tree = parser.parse(source, None).expect("parser returned no tree");
    assert!(
        !tree.root_node().has_error(),
        "scope-flow fixture must not contain parser errors:\n{}",
        tree.root_node().to_sexp()
    );
    tree
}

fn cfg_for<'a>(cfgs: &'a [Cfg], name: &str) -> &'a Cfg {
    cfgs.iter()
        .find(|cfg| cfg.proc_name == name)
        .unwrap_or_else(|| {
            let names: Vec<&str> = cfgs.iter().map(|cfg| cfg.proc_name.as_str()).collect();
            panic!("CFG for {name:?} not found; available procedures: {names:?}")
        })
}

fn cfg_names(cfgs: &[Cfg]) -> Vec<&str> {
    cfgs.iter().map(|cfg| cfg.proc_name.as_str()).collect()
}

fn statement_texts(cfg: &Cfg, source: &[u8]) -> Vec<String> {
    cfg.graph
        .node_indices()
        .flat_map(|index| {
            cfg.graph[index].stmts.iter().filter_map(|stmt| {
                std::str::from_utf8(&source[stmt.byte_range.clone()])
                    .ok()
                    .map(str::to_string)
            })
        })
        .collect()
}

fn block_with_stmt(cfg: &Cfg, source: &[u8], kind: &str, text: &str) -> BlockId {
    cfg.graph
        .node_indices()
        .find_map(|index| {
            let block = &cfg.graph[index];
            block.stmts.iter().find_map(|stmt| {
                let stmt_text = std::str::from_utf8(&source[stmt.byte_range.clone()]).ok()?;
                (stmt.node_kind == kind && stmt_text.contains(text)).then(|| BlockId::from(index))
            })
        })
        .unwrap_or_else(|| panic!("statement {kind:?} containing {text:?} not found"))
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

fn source_span(source: &[u8], start_text: &str, end_text: &str) -> Range<usize> {
    let source_text = std::str::from_utf8(source).expect("fixture must be UTF-8");
    let start = source_text
        .find(start_text)
        .unwrap_or_else(|| panic!("start marker {start_text:?} not found"));
    let end = source_text[start..]
        .find(end_text)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("end marker {end_text:?} not found"));
    start..end
}

#[test]
fn nested_routines_are_collected_with_qualified_names_and_isolated_refs() {
    let source = br#"
unit NestedScopes;
interface
implementation

procedure First;
  procedure Same;
    procedure Deep;
    begin
      FirstDeepWork;
    end;
  begin
    FirstNestedWork;
  end;
begin
  FirstOuterWork;
end;

procedure Second;
  procedure Same;
  begin
    SecondNestedWork;
  end;
begin
  SecondOuterWork;
end;

procedure TClass.Method;
  function Inner: Integer;
  begin
    MethodNestedWork;
  end;
begin
  MethodOuterWork;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    assert_eq!(
        cfg_names(&cfgs),
        vec![
            "First",
            "First.Same",
            "First.Same.Deep",
            "Second",
            "Second.Same",
            "TClass.Method",
            "TClass.Method.Inner",
        ],
        "routine CFGs must use stable lexical pre-order"
    );

    let first = cfg_for(&cfgs, "First");
    let first_same = cfg_for(&cfgs, "First.Same");
    let first_same_deep = cfg_for(&cfgs, "First.Same.Deep");
    let second = cfg_for(&cfgs, "Second");
    let second_same = cfg_for(&cfgs, "Second.Same");
    let method = cfg_for(&cfgs, "TClass.Method");
    let method_inner = cfg_for(&cfgs, "TClass.Method.Inner");

    let first_refs = statement_texts(first, &source).join("\n");
    assert!(first_refs.contains("FirstOuterWork"));
    assert!(!first_refs.contains("FirstNestedWork"));
    assert!(!first_refs.contains("SecondNestedWork"));

    let first_same_refs = statement_texts(first_same, &source).join("\n");
    assert!(first_same_refs.contains("FirstNestedWork"));
    assert!(!first_same_refs.contains("FirstOuterWork"));
    assert!(!first_same_refs.contains("SecondNestedWork"));

    let first_same_deep_refs = statement_texts(first_same_deep, &source).join("\n");
    assert!(first_same_deep_refs.contains("FirstDeepWork"));
    assert!(!first_same_deep_refs.contains("FirstNestedWork"));
    assert!(!first_same_deep_refs.contains("FirstOuterWork"));

    let second_refs = statement_texts(second, &source).join("\n");
    assert!(second_refs.contains("SecondOuterWork"));
    assert!(!second_refs.contains("SecondNestedWork"));
    assert!(!second_refs.contains("FirstNestedWork"));

    let second_same_refs = statement_texts(second_same, &source).join("\n");
    assert!(second_same_refs.contains("SecondNestedWork"));
    assert!(!second_same_refs.contains("SecondOuterWork"));
    assert!(!second_same_refs.contains("FirstNestedWork"));

    let method_refs = statement_texts(method, &source).join("\n");
    assert!(method_refs.contains("MethodOuterWork"));
    assert!(!method_refs.contains("MethodNestedWork"));

    let method_inner_refs = statement_texts(method_inner, &source).join("\n");
    assert!(method_inner_refs.contains("MethodNestedWork"));
    assert!(!method_inner_refs.contains("MethodOuterWork"));

    assert_eq!(
        first.byte_range,
        source_span(&source, "procedure First;", "\n\nprocedure Second")
    );
    assert_eq!(
        first_same.byte_range,
        source_span(&source, "procedure Same;", "\nbegin\n  FirstOuterWork")
    );
    assert_eq!(
        method_inner.byte_range,
        source_span(
            &source,
            "function Inner: Integer;",
            "\nbegin\n  MethodOuterWork"
        )
    );
}

#[test]
fn nested_labels_are_resolved_in_their_own_routine_cfgs() {
    let source = br#"
unit NestedLabels;
interface
implementation

procedure Outer;
  procedure Inner;
  begin
    goto Done;
  Done:
    InnerWork;
  end;
begin
  goto Done;
Done:
  OuterWork;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let outer = cfg_for(&cfgs, "Outer");
    let inner = cfg_for(&cfgs, "Outer.Inner");

    let outer_goto = block_with_stmt(outer, &source, "goto", "goto Done");
    let outer_label = block_with_stmt(outer, &source, "label", "Done:");
    assert_eq!(
        successors(outer, outer_goto),
        vec![(outer_label, EdgeKind::Goto)]
    );

    let inner_goto = block_with_stmt(inner, &source, "goto", "goto Done");
    let inner_label = block_with_stmt(inner, &source, "label", "Done:");
    assert_eq!(
        successors(inner, inner_goto),
        vec![(inner_label, EdgeKind::Goto)]
    );
}

#[test]
fn forward_routine_declarations_do_not_create_body_cfgs() {
    let source = br#"
unit ForwardScopes;
interface
implementation

function ForwardFunction(Value: Integer): Integer; forward;

procedure RealRoutine;
begin
  RealWork;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    assert_eq!(cfg_names(&cfgs), vec!["RealRoutine"]);
}
