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

fn blocks_with_stmt(cfg: &Cfg, source: &[u8], kind: &str, text: &str) -> Vec<BlockId> {
    cfg.graph
        .node_indices()
        .filter_map(|index| {
            let block = &cfg.graph[index];
            block
                .stmts
                .iter()
                .any(|stmt| {
                    stmt.node_kind == kind
                        && std::str::from_utf8(&source[stmt.byte_range.clone()])
                            .is_ok_and(|stmt_text| stmt_text.contains(text))
                })
                .then(|| BlockId::from(index))
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

#[test]
fn foreach_has_a_back_edge_exit_and_nested_loop_controls() {
    let source = br#"
unit ForEachFlow;
interface
implementation

procedure ForEachControls;
begin
  for Item in Items do
  begin
    if Item = 1 then
      Continue;
    if Item = 2 then
      Break;
    Body(Item);
  end;
  AfterEach;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ForEachControls");

    let header = block_with_stmt(cfg, &source, "foreach", "for Item in Items");
    let body = block_with_stmt(cfg, &source, "statement", "Body(Item)");
    let continue_stmt = block_with_stmt(cfg, &source, "statement", "Continue");
    let break_stmt = block_with_stmt(cfg, &source, "statement", "Break");
    let after_each = block_with_stmt(cfg, &source, "statement", "AfterEach");

    let header_successors = successors(cfg, header);
    let body_entry = header_successors
        .iter()
        .find_map(|(target, kind)| (*kind == EdgeKind::ConditionalTrue).then_some(*target))
        .expect("foreach header must enter its body");
    let loop_exit = header_successors
        .iter()
        .find_map(|(target, kind)| (*kind == EdgeKind::LoopExit).then_some(*target))
        .expect("foreach header must have a loop-exit edge");
    assert!(can_reach(cfg, body_entry, body));
    assert!(can_reach(cfg, body, header));
    assert!(can_reach(cfg, loop_exit, after_each));
    assert_eq!(
        successors(cfg, continue_stmt),
        vec![(header, EdgeKind::Normal)],
        "Continue inside a foreach must restart the foreach"
    );
    assert_eq!(
        successors(cfg, break_stmt),
        vec![(loop_exit, EdgeKind::Normal)],
        "Break inside a foreach must leave the foreach"
    );

    let header_ref = cfg.graph[header.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "foreach")
        .expect("foreach header reference");
    let header_text = std::str::from_utf8(&source[header_ref.byte_range.clone()]).unwrap();
    assert!(!header_text.contains("Body(Item)"));
}

#[test]
fn foreach_header_and_body_exceptions_reach_an_enclosing_handler() {
    let source = br#"
unit ProtectedForEach;
interface
implementation

procedure ProtectedForEach;
begin
  try
    for Item in ItemsCall() do
      BodyCall(Item);
  except
    HandleEach;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ProtectedForEach");

    let header = block_with_stmt(cfg, &source, "foreach", "for Item in ItemsCall()");
    let body = block_with_stmt(cfg, &source, "statement", "BodyCall(Item)");
    let handler = block_with_stmt(cfg, &source, "statement", "HandleEach");
    assert!(successors(cfg, header).contains(&(handler, EdgeKind::ExceptionThrow)));
    assert!(successors(cfg, body).contains(&(handler, EdgeKind::ExceptionThrow)));

    let header_ref = cfg.graph[header.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "foreach")
        .expect("foreach header reference");
    let header_text = std::str::from_utf8(&source[header_ref.byte_range.clone()]).unwrap();
    assert!(!header_text.contains("BodyCall"));
}

#[test]
fn with_evaluates_context_once_and_walks_a_nested_body() {
    let source = br#"
unit WithFlow;
interface
implementation

procedure NestedWith;
begin
  with ContextRecord(), OtherContext() do
  begin
    if Ready then
      NestedBody
    else
      AlternateBody;
  end;
  AfterWith;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "NestedWith");

    let context = block_with_stmt(cfg, &source, "with", "with ContextRecord(), OtherContext()");
    let nested_body = block_with_stmt(cfg, &source, "statement", "NestedBody");
    let alternate_body = block_with_stmt(cfg, &source, "statement", "AlternateBody");
    let after_with = block_with_stmt(cfg, &source, "statement", "AfterWith");

    let body_entry = successors(cfg, context)
        .into_iter()
        .find_map(|(target, kind)| (kind == EdgeKind::Normal).then_some(target))
        .expect("with context evaluation must enter its body");
    assert!(can_reach(cfg, body_entry, nested_body));
    assert!(can_reach(cfg, body_entry, alternate_body));
    assert!(can_reach(cfg, nested_body, after_with));
    assert!(can_reach(cfg, alternate_body, after_with));

    let context_ref = cfg.graph[context.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "with")
        .expect("with context reference");
    let context_text = std::str::from_utf8(&source[context_ref.byte_range.clone()]).unwrap();
    assert!(!context_text.contains("NestedBody"));
    assert!(!context_text.contains("AlternateBody"));
}

#[test]
fn with_context_and_body_exceptions_reach_an_enclosing_handler() {
    let source = br#"
unit ProtectedWith;
interface
implementation

procedure ProtectedWith;
begin
  try
    with ContextCall() do
      BodyCall;
  except
    HandleWith;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ProtectedWith");

    let context = block_with_stmt(cfg, &source, "with", "with ContextCall()");
    let body = block_with_stmt(cfg, &source, "statement", "BodyCall");
    let handler = block_with_stmt(cfg, &source, "statement", "HandleWith");
    assert!(successors(cfg, context).contains(&(handler, EdgeKind::ExceptionThrow)));
    assert!(successors(cfg, body).contains(&(handler, EdgeKind::ExceptionThrow)));

    let context_ref = cfg.graph[context.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "with")
        .expect("with context reference");
    let context_text = std::str::from_utf8(&source[context_ref.byte_range.clone()]).unwrap();
    assert!(!context_text.contains("BodyCall"));
}

#[test]
fn goto_resolves_forward_backward_case_insensitive_and_stops_fallthrough() {
    let source = br#"
unit GotoFlow;
interface
implementation

procedure GotoTargets;
begin
  goto ForwardLabel;
  SkippedForward;
ForwardLabel:
  ForwardBody;
  goto BackLabel;
BackLabel:
  BackBody;
  goto FORWARDLABEL;
  UnreachableAfterBackward;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "GotoTargets");

    let forward_goto = block_with_stmt(cfg, &source, "goto", "goto ForwardLabel");
    let second_goto = block_with_stmt(cfg, &source, "goto", "goto BackLabel");
    let backward_goto = block_with_stmt(cfg, &source, "goto", "goto FORWARDLABEL");
    let forward_body = block_with_stmt(cfg, &source, "statement", "ForwardBody");
    let back_body = block_with_stmt(cfg, &source, "statement", "BackBody");
    let skipped = block_with_stmt(cfg, &source, "statement", "SkippedForward");
    let unreachable = block_with_stmt(cfg, &source, "statement", "UnreachableAfterBackward");

    assert_eq!(
        successors(cfg, forward_goto),
        vec![(forward_body, EdgeKind::Goto)],
        "forward goto must target the labeled statement without fallthrough"
    );
    assert_eq!(
        successors(cfg, second_goto),
        vec![(back_body, EdgeKind::Goto)],
        "goto target names must be resolved case-insensitively"
    );
    assert_eq!(
        successors(cfg, backward_goto),
        vec![(forward_body, EdgeKind::Goto)],
        "backward goto must resolve to an earlier label"
    );
    assert!(!can_reach(cfg, forward_goto, skipped));
    assert!(!can_reach(cfg, backward_goto, unreachable));
    assert!(can_reach(cfg, forward_body, back_body));
    assert!(can_reach(cfg, back_body, forward_body));
}

#[test]
fn goto_inside_finally_scope_does_not_unwind_but_leaving_goto_does() {
    let source = br#"
unit GotoCleanup;
interface
implementation

procedure SameScopeGoto;
begin
  try
    goto InsideLabel;
    SkippedInside;
InsideLabel:
    InsideBody;
  finally
    SameCleanup;
  end;
end;

procedure LeavingGoto;
begin
  try
    goto OutsideLabel;
  finally
    LeavingCleanup;
  end;
OutsideLabel:
  OutsideBody;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let same_cfg = cfg_for(&cfgs, "SameScopeGoto");
    let same_goto = block_with_stmt(same_cfg, &source, "goto", "goto InsideLabel");
    let inside_label = block_with_stmt(same_cfg, &source, "label", "InsideLabel");
    let inside_body = block_with_stmt(same_cfg, &source, "statement", "InsideBody");
    assert_eq!(
        successors(same_cfg, same_goto),
        vec![(inside_label, EdgeKind::Goto)],
        "a goto whose label remains in the try body must not enter its finally"
    );
    assert!(can_reach(same_cfg, inside_label, inside_body));

    let leaving_cfg = cfg_for(&cfgs, "LeavingGoto");
    let leaving_goto = block_with_stmt(leaving_cfg, &source, "goto", "goto OutsideLabel");
    let leaving_cleanup = block_with_stmt(leaving_cfg, &source, "statement", "LeavingCleanup");
    let outside_body = block_with_stmt(leaving_cfg, &source, "statement", "OutsideBody");
    assert!(
        successors(leaving_cfg, leaving_goto).contains(&(leaving_cleanup, EdgeKind::FinallyEntry)),
        "a goto leaving a finally scope must enter its cleanup"
    );
    assert!(
        successors(leaving_cfg, leaving_cleanup).contains(&(outside_body, EdgeKind::FinallyExit))
    );
    assert!(!successors(leaving_cfg, leaving_goto)
        .iter()
        .any(|(target, kind)| *target == outside_body && *kind == EdgeKind::Goto));
}

#[test]
fn goto_finalizer_clones_keep_distinct_label_continuations() {
    let source = br#"
unit GotoFinalizerLabels;
interface
implementation

procedure DistinctGotoLabels;
begin
  try
    if ChooseFirst then
      goto FirstLabel
    else
      goto SecondLabel;
  finally
    CleanupBranch;
  end;
FirstLabel:
  FirstBody;
  Exit;
SecondLabel:
  SecondBody;
  Exit;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "DistinctGotoLabels");

    let first_body = block_with_stmt(cfg, &source, "statement", "FirstBody");
    let second_body = block_with_stmt(cfg, &source, "statement", "SecondBody");
    let cleanup_blocks = blocks_with_stmt(cfg, &source, "statement", "CleanupBranch");
    assert!(
        cleanup_blocks.len() >= 2,
        "different goto labels need distinct finalizer continuations"
    );

    let first_cleanup = cleanup_blocks
        .iter()
        .copied()
        .find(|cleanup| successors(cfg, *cleanup).contains(&(first_body, EdgeKind::FinallyExit)))
        .expect("one finalizer clone must continue to FirstLabel");
    let second_cleanup = cleanup_blocks
        .iter()
        .copied()
        .find(|cleanup| successors(cfg, *cleanup).contains(&(second_body, EdgeKind::FinallyExit)))
        .expect("one finalizer clone must continue to SecondLabel");
    assert_ne!(first_cleanup, second_cleanup);
    assert!(!can_reach(cfg, first_cleanup, second_body));
    assert!(!can_reach(cfg, second_cleanup, first_body));
}

#[test]
fn case_selector_reference_skips_parser_extras() {
    let source = br#"
unit CaseSelectorExtra;
interface
implementation

procedure CaseSelectorExtra;
begin
  case { rationale } SelectorCall() of
    1: Arm;
  else
    DefaultArm;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "CaseSelectorExtra");
    let selector = block_with_stmt(cfg, &source, "case", "case");
    let selector_ref = cfg.graph[selector.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "case")
        .expect("case selector reference");
    let selector_text = std::str::from_utf8(&source[selector_ref.byte_range.clone()]).unwrap();
    assert!(selector_text.contains("SelectorCall()"));
}

#[test]
fn finally_goto_resolves_a_procedure_level_label() {
    let source = br#"
unit FinallyGotoProcedureLabel;
interface
implementation

procedure FinallyGotoProcedureLabel;
label Done;
begin
  try
    Work;
  finally
    goto Done;
  end;
  Skipped;
Done:
  TargetBody;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "FinallyGotoProcedureLabel");
    let label = block_with_stmt(cfg, &source, "label", "Done:");
    let gotos = blocks_with_stmt(cfg, &source, "goto", "goto Done");
    assert!(gotos.len() >= 2);
    for goto in gotos {
        assert_eq!(
            successors(cfg, goto),
            vec![(label, EdgeKind::FinallyExit)],
            "outward finalizer goto must resolve the procedure label"
        );
    }
}

#[test]
fn nested_finally_goto_resolves_an_enclosing_finalizer_label() {
    let source = br#"
unit NestedFinallyGoto;
interface
implementation

procedure NestedFinallyGoto;
label Done;
begin
  try
    Exit;
  finally
    try
      NestedWork;
    finally
      goto Done;
    end;
Done:
    Cleanup;
  end;
  After;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "NestedFinallyGoto");
    let label = block_with_stmt(cfg, &source, "label", "Done:");
    let gotos = blocks_with_stmt(cfg, &source, "goto", "goto Done");
    assert!(gotos.len() >= 2);
    for goto in gotos {
        assert_eq!(
            successors(cfg, goto),
            vec![(label, EdgeKind::FinallyExit)],
            "nested finalizer goto must resolve its enclosing label"
        );
    }
}

#[test]
fn cloned_cleanup_gotos_stay_in_their_own_instance() {
    let source = br#"
unit ClonedCleanupGotos;
interface
implementation

procedure ClonedCleanupGotos;
begin
  try
    if LeaveNow then
      Exit;
    Work;
  finally
    goto Done;
  Done:
    Cleanup;
  end;
  After;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ClonedCleanupGotos");
    let gotos = blocks_with_stmt(cfg, &source, "goto", "goto Done");
    let labels = blocks_with_stmt(cfg, &source, "label", "Done:");
    assert_eq!(gotos.len(), labels.len());
    assert!(gotos.len() >= 2);
    for (goto, label) in gotos.into_iter().zip(labels) {
        assert!(
            successors(cfg, goto)
                .iter()
                .any(|(target, _)| *target == label),
            "goto {goto:?} must target its same-clone label {label:?}"
        );
    }
}

#[test]
fn goto_after_nested_cleanup_clones_retains_the_later_scope_identity() {
    let source = br#"
unit LaterScopeIdentity;
interface
implementation

procedure LaterScopeIdentity;
begin
  try
    Work;
  finally
    try
      NestedWork;
    finally
      NestedCleanup;
    end;
  end;
  try
    goto Done;
  Done:
    TargetBody;
  finally
    LaterCleanup;
  end;
  After;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "LaterScopeIdentity");
    let goto = block_with_stmt(cfg, &source, "goto", "goto Done");
    let label = block_with_stmt(cfg, &source, "label", "Done:");
    assert_eq!(
        successors(cfg, goto),
        vec![(label, EdgeKind::Goto)],
        "a same-scope goto must not enter the later finally"
    );
}
