use std::collections::HashSet;

use cfg_core::{BasicBlockKind, BlockId, Cfg, EdgeKind};
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
        "exception-flow fixture must not contain parser errors:\n{}",
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

fn block_with_stmt(cfg: &Cfg, source: &[u8], kind: &str, text: &str) -> BlockId {
    let blocks = blocks_with_stmt(cfg, source, kind, text);
    assert_eq!(
        blocks.len(),
        1,
        "expected one {kind:?} statement containing {text:?}, got {blocks:?}"
    );
    blocks[0]
}

fn block_of_kind(cfg: &Cfg, kind: BasicBlockKind) -> BlockId {
    cfg.graph
        .node_indices()
        .find_map(|index| (cfg.graph[index].kind == kind).then(|| BlockId::from(index)))
        .unwrap_or_else(|| panic!("block of kind {kind:?} not found"))
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
fn finally_preserves_normal_and_exit_continuations() {
    let source = br#"
unit FinallyContinuations;
interface
implementation

procedure NormalFinally;
begin
  try
    Work;
  finally
    CleanupNormal;
  end;
  AfterNormal;
end;

procedure ExitFinally;
begin
  try
    Exit;
  finally
    CleanupForReturn;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let normal_cfg = cfg_for(&cfgs, "NormalFinally");
    let work = block_with_stmt(normal_cfg, &source, "statement", "Work");
    let cleanup_normal = blocks_with_stmt(normal_cfg, &source, "statement", "CleanupNormal");
    let after_normal = block_with_stmt(normal_cfg, &source, "statement", "AfterNormal");
    assert!(
        cleanup_normal.len() >= 2,
        "normal and exceptional completion need distinct cleanup paths"
    );
    let normal_cleanup = cleanup_normal
        .iter()
        .copied()
        .find(|block| can_reach(normal_cfg, *block, after_normal))
        .expect("normal completion must reach its cleanup and the after block");
    assert!(can_reach(normal_cfg, work, normal_cleanup));
    assert!(can_reach(normal_cfg, normal_cleanup, after_normal));

    let exit_cfg = cfg_for(&cfgs, "ExitFinally");
    let exit_stmt = block_with_stmt(exit_cfg, &source, "statement", "Exit;");
    let cleanup_exit = block_with_stmt(exit_cfg, &source, "statement", "CleanupForReturn");
    assert!(
        can_reach(exit_cfg, exit_stmt, cleanup_exit),
        "Exit must enter finally before leaving the procedure"
    );
}

#[test]
fn break_and_continue_each_use_their_own_finally_continuation() {
    let source = br#"
unit LoopFinally;
interface
implementation

procedure LoopFinally;
var I: Integer;
begin
  while I < 10 do
  begin
    try
      if I = 0 then
        Break
      else
        Continue;
    finally
      CleanupLoop;
    end;
  end;
  AfterLoop;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "LoopFinally");

    let condition = block_with_stmt(cfg, &source, "while", "while I < 10");
    let after_loop = successors(cfg, condition)
        .into_iter()
        .find_map(|(target, kind)| (kind == EdgeKind::LoopExit).then_some(target))
        .expect("while condition must have a loop-exit successor");
    let break_stmt = block_with_stmt(cfg, &source, "statement", "Break");
    let continue_stmt = block_with_stmt(cfg, &source, "statement", "Continue");
    let cleanup_blocks = blocks_with_stmt(cfg, &source, "statement", "CleanupLoop");
    assert!(
        cleanup_blocks.len() >= 3,
        "normal, loop-transfer, and exceptional paths need cleanup"
    );

    let break_cleanup = cleanup_blocks
        .iter()
        .copied()
        .find(|block| {
            successors(cfg, break_stmt)
                .iter()
                .any(|(target, kind)| *target == *block && *kind == EdgeKind::FinallyEntry)
        })
        .expect("Break must reach a finally body");
    let continue_cleanup = cleanup_blocks
        .iter()
        .copied()
        .find(|block| {
            successors(cfg, continue_stmt)
                .iter()
                .any(|(target, kind)| *target == *block && *kind == EdgeKind::FinallyEntry)
        })
        .expect("Continue must reach a finally body");
    assert_ne!(break_cleanup, continue_cleanup);
    assert_eq!(
        successors(cfg, break_stmt),
        vec![(break_cleanup, EdgeKind::FinallyEntry)],
        "Break must have only its own cleanup successor"
    );
    assert_eq!(
        successors(cfg, continue_stmt),
        vec![(continue_cleanup, EdgeKind::FinallyEntry)],
        "Continue must have only its own cleanup successor"
    );
    let break_cleanup_successors = successors(cfg, break_cleanup);
    assert!(break_cleanup_successors.contains(&(after_loop, EdgeKind::FinallyExit)));
    assert!(!break_cleanup_successors.contains(&(condition, EdgeKind::FinallyExit)));

    let continue_cleanup_successors = successors(cfg, continue_cleanup);
    assert!(continue_cleanup_successors.contains(&(condition, EdgeKind::FinallyExit)));
    assert!(!continue_cleanup_successors.contains(&(after_loop, EdgeKind::FinallyExit)));
}

#[test]
fn exception_from_inner_try_finally_reaches_outer_handler() {
    let source = br#"
unit NestedFinallyException;
interface
implementation

procedure NestedFinallyException;
begin
  try
    try
      raise Exception.Create('boom');
    finally
      InnerCleanup;
    end;
  except
    OuterHandled;
  end;
  AfterOuter;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "NestedFinallyException");

    let cleanup = block_with_stmt(cfg, &source, "statement", "InnerCleanup");
    let outer_handler = block_of_kind(cfg, BasicBlockKind::BareExceptHandler);
    assert!(
        successors(cfg, cleanup)
            .iter()
            .all(|(target, kind)| *target == outer_handler && *kind == EdgeKind::ExceptionThrow),
        "an exception pending through inner finally must reach the outer handler"
    );
}

#[test]
fn exit_from_inner_finally_unwinds_an_outer_finally() {
    let source = br#"
unit NestedFinallyExit;
interface
implementation

procedure NestedFinallyExit;
begin
  try
    try
      Work;
    finally
      Exit;
    end;
  finally
    OuterCleanup;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "NestedFinallyExit");

    let exit_stmts = blocks_with_stmt(cfg, &source, "statement", "Exit;");
    let outer_cleanups = blocks_with_stmt(cfg, &source, "statement", "OuterCleanup");
    assert!(!exit_stmts.is_empty());
    assert!(!outer_cleanups.is_empty());
    for exit_stmt in exit_stmts {
        assert!(
            outer_cleanups
                .iter()
                .any(|cleanup| can_reach(cfg, exit_stmt, *cleanup)),
            "Exit from an inner finalizer must unwind the outer finalizer"
        );
    }
}
