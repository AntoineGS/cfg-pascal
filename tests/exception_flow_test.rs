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

    let cleanups = blocks_with_stmt(cfg, &source, "statement", "InnerCleanup");
    let outer_handler = block_of_kind(cfg, BasicBlockKind::BareExceptHandler);
    assert!(cleanups.len() >= 2);
    for cleanup in cleanups {
        assert!(
            successors(cfg, cleanup)
                .iter()
                .all(|(target, kind)| *target == outer_handler
                    && *kind == EdgeKind::ExceptionThrow),
            "an exception pending through inner finally must reach the outer handler"
        );
    }
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

#[test]
fn typed_handlers_are_alternatives_and_unknown_exceptions_propagate() {
    let source = br#"
unit TypedHandlers;
interface
implementation

procedure TypedAlternatives;
begin
  try
    raise E;
  except
    on E: FirstException do
      FirstHandler;
    on E: SecondException do
      SecondHandler;
  end;
  AfterTyped;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "TypedAlternatives");

    let raise_stmt = block_with_stmt(cfg, &source, "raise", "raise E");
    let first_handler = block_with_stmt(cfg, &source, "statement", "FirstHandler");
    let second_handler = block_with_stmt(cfg, &source, "statement", "SecondHandler");
    let after = block_with_stmt(cfg, &source, "statement", "AfterTyped");
    let raise_successors = successors(cfg, raise_stmt);

    assert!(raise_successors.contains(&(first_handler, EdgeKind::ExceptionThrow)));
    assert!(raise_successors.contains(&(second_handler, EdgeKind::ExceptionThrow)));
    assert!(raise_successors.contains(&(cfg.exit, EdgeKind::ExceptionThrow)));
    assert!(!can_reach(cfg, first_handler, second_handler));
    assert!(!can_reach(cfg, second_handler, first_handler));
    assert!(can_reach(cfg, first_handler, after));
    assert!(can_reach(cfg, second_handler, after));
}

#[test]
fn exception_constructor_spelling_keeps_base_handler_alternative() {
    let source = br#"
unit ExceptionTypeAlternatives;
interface
implementation

procedure FunctionRaised;
begin
  try
    raise MakeError();
  except
    on E: Exception do
      HandleFunctionError;
  end;
end;

procedure QualifiedSubclassRaised;
begin
  try
    raise EArgumentException.Create('bad');
  except
    on E: Exception do
      HandleSubclassError;
  end;
end;

procedure QualifiedBaseRaised;
begin
  try
    raise SysUtils.Exception.Create('bad');
  except
    on E: SysUtils.Exception do
      HandleQualifiedError;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    for (procedure, raise_text, handler_text) in [
        ("FunctionRaised", "raise MakeError", "HandleFunctionError"),
        (
            "QualifiedSubclassRaised",
            "raise EArgumentException.Create",
            "HandleSubclassError",
        ),
        (
            "QualifiedBaseRaised",
            "raise SysUtils.Exception.Create",
            "HandleQualifiedError",
        ),
    ] {
        let cfg = cfg_for(&cfgs, procedure);
        let raise_stmt = block_with_stmt(cfg, &source, "raise", raise_text);
        let handler = block_with_stmt(cfg, &source, "statement", handler_text);
        assert!(
            successors(cfg, raise_stmt).contains(&(handler, EdgeKind::ExceptionThrow)),
            "{raise_text} must retain the typed handler as a conservative alternative"
        );
    }
}

#[test]
fn exception_constructor_argument_keeps_unknown_handler_alternative() {
    let source = br#"
unit ExceptionConstructorArgument;
interface
implementation

procedure ConstructorArgument;
begin
  try
    raise EOne.Create(ThrowingValue());
  except
    on E: EOne do
      HandleOne;
    on E: ETwo do
      HandleTwo;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ConstructorArgument");
    let raise_stmt = block_with_stmt(cfg, &source, "raise", "raise EOne.Create");
    let one_handler = block_with_stmt(cfg, &source, "statement", "HandleOne");
    let two_handler = block_with_stmt(cfg, &source, "statement", "HandleTwo");
    let raise_successors = successors(cfg, raise_stmt);

    assert!(raise_successors.contains(&(one_handler, EdgeKind::ExceptionThrow)));
    assert!(
        raise_successors.contains(&(two_handler, EdgeKind::ExceptionThrow)),
        "constructor argument evaluation must retain unknown exception handlers"
    );
}

#[test]
fn exception_constructor_evaluation_enters_finally_as_unknown_transfer() {
    let source = br#"
unit ExceptionConstructorFinally;
interface
implementation

procedure ConstructorFinally;
begin
  try
    raise EOne.Create(ThrowingValue());
  finally
    CleanupConstructor;
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ConstructorFinally");
    let cleanup_blocks = blocks_with_stmt(cfg, &source, "statement", "CleanupConstructor");

    assert!(
        cleanup_blocks.len() >= 2,
        "explicit exception and constructor evaluation need distinct pending paths"
    );
}

#[test]
fn plain_except_and_exception_else_walk_all_handler_statements() {
    let source = br#"
unit BareHandlers;
interface
implementation

procedure PlainExcept;
begin
  try
    WorkPlain;
  except
    PlainFirst;
    PlainSecond;
  end;
  AfterPlain;
end;

procedure ExceptionElse;
begin
  try
    WorkElse;
  except
    on E: KnownException do
      TypedElse;
  else
    ElseFirst;
    ElseSecond;
  end;
  AfterElse;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let plain_cfg = cfg_for(&cfgs, "PlainExcept");
    let plain_first = block_with_stmt(plain_cfg, &source, "statement", "PlainFirst");
    let plain_second = block_with_stmt(plain_cfg, &source, "statement", "PlainSecond");
    let after_plain = block_with_stmt(plain_cfg, &source, "statement", "AfterPlain");
    assert!(can_reach(plain_cfg, plain_first, plain_second));
    assert!(can_reach(plain_cfg, plain_second, after_plain));
    assert_eq!(
        plain_cfg.graph[plain_first.index()].kind,
        BasicBlockKind::BareExceptHandler
    );

    let else_cfg = cfg_for(&cfgs, "ExceptionElse");
    let else_first = block_with_stmt(else_cfg, &source, "statement", "ElseFirst");
    let else_second = block_with_stmt(else_cfg, &source, "statement", "ElseSecond");
    let typed_else = block_with_stmt(else_cfg, &source, "statement", "TypedElse");
    let after_else = block_with_stmt(else_cfg, &source, "statement", "AfterElse");
    let work_else = block_with_stmt(else_cfg, &source, "statement", "WorkElse");
    let bare_handler = else_cfg
        .graph
        .node_indices()
        .find_map(|index| {
            let block = &else_cfg.graph[index];
            (block.kind == BasicBlockKind::BareExceptHandler).then(|| BlockId::from(index))
        })
        .expect("exceptionElse must have a bare handler block");

    assert!(can_reach(else_cfg, else_first, else_second));
    assert!(can_reach(else_cfg, else_second, after_else));
    assert!(can_reach(else_cfg, typed_else, after_else));
    assert!(
        successors(else_cfg, work_else).contains(&(bare_handler, EdgeKind::ExceptionThrow)),
        "unknown protected exceptions must dispatch to exceptionElse"
    );
}

#[test]
fn implicit_exceptions_attach_to_protected_statements_and_conditions_only() {
    let source = br#"
unit ImplicitExceptions;
interface
implementation

procedure ProtectedStatements;
begin
  BeforeUnprotected();
  try
    if ConditionCall() then
      ThenCall()
    else
      ElseCall();
    while LoopConditionCall() do
      LoopBodyCall();
    AfterProtectedCall();
  except
    HandleProtected();
  end;
  AfterTryCall();
end;

procedure HandlerThrows;
begin
  try
    raise Exception.Create('body');
  except
    HandlerThrowsCall();
  end;
end;

procedure FinalizerThrows;
begin
  try
    ProtectedWork();
  finally
    FinalizerThrowsCall();
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let cfg = cfg_for(&cfgs, "ProtectedStatements");
    let handler = block_with_stmt(cfg, &source, "statement", "HandleProtected");
    let before = block_with_stmt(cfg, &source, "statement", "BeforeUnprotected");
    let condition = block_with_stmt(cfg, &source, "ifElse", "if ConditionCall");
    let then_call = block_with_stmt(cfg, &source, "statement", "ThenCall");
    let else_call = block_with_stmt(cfg, &source, "statement", "ElseCall");
    let loop_condition = block_with_stmt(cfg, &source, "while", "while LoopConditionCall");
    let loop_body = block_with_stmt(cfg, &source, "statement", "LoopBodyCall");
    let after_loop = block_with_stmt(cfg, &source, "statement", "AfterProtectedCall");

    assert!(
        !successors(cfg, before)
            .iter()
            .any(|(_, kind)| *kind == EdgeKind::ExceptionThrow),
        "unprotected code before try must not enter the inner handler"
    );
    for protected_block in [
        condition,
        then_call,
        else_call,
        loop_condition,
        loop_body,
        after_loop,
    ] {
        assert!(
            successors(cfg, protected_block).contains(&(handler, EdgeKind::ExceptionThrow)),
            "protected block {protected_block:?} must have an exceptional handler edge"
        );
    }

    let condition_text = cfg.graph[condition.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "ifElse")
        .map(|stmt| std::str::from_utf8(&source[stmt.byte_range.clone()]).unwrap())
        .expect("if condition statement reference");
    assert!(!condition_text.contains("ThenCall"));
    assert!(!condition_text.contains("ElseCall"));

    let loop_text = cfg.graph[loop_condition.index()]
        .stmts
        .iter()
        .find(|stmt| stmt.node_kind == "while")
        .map(|stmt| std::str::from_utf8(&source[stmt.byte_range.clone()]).unwrap())
        .expect("while condition statement reference");
    assert!(!loop_text.contains("LoopBodyCall"));
    assert!(!loop_text.contains("AfterProtectedCall"));
}

#[test]
fn handler_and_finalizer_calls_have_outward_exception_edges() {
    let source = br#"
unit OutwardExceptions;
interface
implementation

procedure HandlerThrows;
begin
  try
    raise Exception.Create('body');
  except
    HandlerThrowsCall();
  end;
end;

procedure FinalizerThrows;
begin
  try
    ProtectedWork();
  finally
    FinalizerThrowsCall();
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);

    let handler_cfg = cfg_for(&cfgs, "HandlerThrows");
    let handler_call = block_with_stmt(handler_cfg, &source, "statement", "HandlerThrowsCall");
    assert!(
        successors(handler_cfg, handler_call)
            .iter()
            .any(|(target, kind)| *target == handler_cfg.exit && *kind == EdgeKind::ExceptionThrow),
        "calls in handlers must propagate outward"
    );

    let finalizer_cfg = cfg_for(&cfgs, "FinalizerThrows");
    let finalizer_calls =
        blocks_with_stmt(finalizer_cfg, &source, "statement", "FinalizerThrowsCall");
    assert!(!finalizer_calls.is_empty());
    for finalizer_call in finalizer_calls {
        assert!(
            successors(finalizer_cfg, finalizer_call)
                .iter()
                .any(|(target, kind)| *target == finalizer_cfg.exit
                    && *kind == EdgeKind::ExceptionThrow),
            "calls in finalizers must propagate outward"
        );
    }
}

#[test]
fn exit_argument_exception_is_preserved_through_finally() {
    let source = br#"
unit ExitArgumentException;
interface
implementation

procedure ExitArgumentException;
begin
  try
    Exit(ThrowingValue());
  finally
    CleanupExitArgument();
  end;
end;

end.
"#
    .to_vec();
    let tree = parse_clean(&source);
    let cfgs = build_file_cfgs(&tree, &source);
    let cfg = cfg_for(&cfgs, "ExitArgumentException");

    let exit_stmt = block_with_stmt(cfg, &source, "statement", "Exit(ThrowingValue");
    let cleanup_blocks = blocks_with_stmt(cfg, &source, "statement", "CleanupExitArgument");
    assert!(
        cleanup_blocks.len() >= 2,
        "Exit completion and argument evaluation need separate cleanup paths"
    );
    let cleanup_targets: HashSet<BlockId> = cleanup_blocks.into_iter().collect();
    let cleanup_successors: Vec<(BlockId, EdgeKind)> = successors(cfg, exit_stmt)
        .into_iter()
        .filter(|(target, _)| cleanup_targets.contains(target))
        .collect();
    assert!(
        cleanup_successors
            .iter()
            .any(|(_, kind)| *kind == EdgeKind::FinallyEntry),
        "the Exit transfer must enter finally"
    );
    assert!(
        cleanup_successors
            .iter()
            .any(|(_, kind)| *kind == EdgeKind::ExceptionThrow),
        "an exception while evaluating Exit's argument must enter finally"
    );
}
