use std::collections::HashSet;

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
        "structured-flow fixture must not contain parser errors:\n{}",
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

fn can_reach(cfg: &Cfg, from: BlockId, to: BlockId) -> bool {
    let mut pending = vec![from];
    let mut visited = HashSet::new();

    while let Some(block) = pending.pop() {
        if block == to {
            return true;
        }
        if visited.insert(block) {
            pending.extend(cfg.graph.neighbors(block.index()).map(BlockId::from));
        }
    }

    false
}

#[test]
fn case_labels_and_ranges_dispatch_to_independent_arms_and_default() {
    let source = br#"
unit CaseFlow;
interface
implementation

procedure CaseAlternatives;
begin
  case Choice of
    1, 2: FirstArm;
    3..5: RangeArm;
  else
    DefaultArm;
  end;
  AfterCase;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "CaseAlternatives");

    let selector = block_with_stmt(cfg, &source, "case", "case Choice");
    let first_arm = block_with_stmt(cfg, &source, "statement", "FirstArm");
    let range_arm = block_with_stmt(cfg, &source, "statement", "RangeArm");
    let default_arm = block_with_stmt(cfg, &source, "statement", "DefaultArm");
    let after_case = block_with_stmt(cfg, &source, "statement", "AfterCase");

    let selector_successors = successors(cfg, selector);
    assert_eq!(
        selector_successors
            .iter()
            .filter(|(_, kind)| *kind == EdgeKind::CaseArm)
            .count(),
        3,
        "the selector must dispatch to both labels/ranges and the default arm"
    );
    for arm in [first_arm, range_arm, default_arm] {
        assert!(
            selector_successors.contains(&(arm, EdgeKind::CaseArm)),
            "selector must have a CaseArm edge to {arm:?}"
        );
        assert!(can_reach(cfg, arm, after_case));
    }
    assert!(!can_reach(cfg, first_arm, range_arm));
    assert!(!can_reach(cfg, range_arm, default_arm));

    let selector_ref = cfg.graph[selector.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "case")
        .expect("case selector reference");
    let selector_text = std::str::from_utf8(&source[selector_ref.byte_range.clone()]).unwrap();
    assert!(!selector_text.contains("FirstArm"));
    assert!(!selector_text.contains("RangeArm"));
    assert!(!selector_text.contains("DefaultArm"));
}

#[test]
fn case_without_default_keeps_a_no_match_path_and_routes_selector_exceptions() {
    let source = br#"
unit CaseNoDefault;
interface
implementation

procedure NestedCase;
begin
  try
    if ChooseCase then
      case CaseValue() of
        1: OneArm;
        2..4: RangeArm;
      end
    else
      ElseArm;
  except
    HandleCase;
  end;
  AfterCase;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "NestedCase");

    let selector = block_with_stmt(cfg, &source, "case", "case CaseValue()");
    let one_arm = block_with_stmt(cfg, &source, "statement", "OneArm");
    let range_arm = block_with_stmt(cfg, &source, "statement", "RangeArm");
    let else_arm = block_with_stmt(cfg, &source, "statement", "ElseArm");
    let handler = block_with_stmt(cfg, &source, "statement", "HandleCase");
    let after_case = block_with_stmt(cfg, &source, "statement", "AfterCase");

    let selector_successors = successors(cfg, selector);
    assert!(selector_successors.contains(&(one_arm, EdgeKind::CaseArm)));
    assert!(selector_successors.contains(&(range_arm, EdgeKind::CaseArm)));
    let no_match = selector_successors
        .iter()
        .find_map(|(target, kind)| {
            (*kind == EdgeKind::CaseArm && *target != one_arm && *target != range_arm)
                .then_some(*target)
        })
        .expect("case without else must retain a no-match CaseArm edge");
    assert!(can_reach(cfg, no_match, after_case));
    assert!(
        selector_successors.contains(&(handler, EdgeKind::ExceptionThrow)),
        "evaluating a case selector must retain its enclosing handler edge"
    );
    assert!(can_reach(cfg, one_arm, after_case));
    assert!(can_reach(cfg, range_arm, after_case));
    assert!(can_reach(cfg, else_arm, after_case));
}
