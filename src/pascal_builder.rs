use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use cfg_core::{BasicBlockKind, BlockId, Cfg, CfgBuildSink, DefaultCfgBuilder, EdgeKind, StmtRef};
use tree_sitter::Node;

use crate::constructs::{
    exit_has_argument, is_break_call, is_continue_call, is_exit_call, node_text,
    raise_may_throw_during_evaluation, raised_exception_type, Flow, LoopFrame, PendingTransfer,
    ScopeId, TransferKind,
};

/// Build CFGs for all procedure/function definitions in a parsed Pascal file.
///
/// Walks the tree looking for `defProc` nodes, extracts the procedure name
/// and body block, then builds a CFG for each one.
pub fn build_file_cfgs(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<Cfg> {
    let mut cfgs = Vec::new();
    collect_def_proc_cfgs(tree.root_node(), source, &mut cfgs);
    cfgs
}

fn collect_def_proc_cfgs(node: Node, source: &[u8], out: &mut Vec<Cfg>) {
    if node.kind() == "defProc" {
        if let Some(cfg) = build_proc_cfg(node, source) {
            out.push(cfg);
        }
        // Nested procedures are deliberately left for Task 4. Do not execute
        // their bodies as part of the containing routine's CFG.
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_def_proc_cfgs(child, source, out);
    }
}

/// Build a CFG for a single `defProc` node.
fn build_proc_cfg(def_proc: Node, source: &[u8]) -> Option<Cfg> {
    let proc_name = extract_proc_name(def_proc, source)?;
    let block = def_proc.child_by_field_name("body")?;

    let byte_range = def_proc.start_byte()..def_proc.end_byte();
    let mut builder = DefaultCfgBuilder::new(proc_name, byte_range);

    let entry = builder.new_block(BasicBlockKind::Entry);
    let exit = builder.new_block(BasicBlockKind::Exit);
    builder.set_entry(entry);
    builder.set_exit(exit);

    let body = builder.new_block(BasicBlockKind::Normal);
    builder.add_edge(entry, body, EdgeKind::Normal);

    let mut label_scopes = HashMap::new();
    let mut label_scope_id = 0;
    collect_label_scopes(block, source, &[], &mut label_scope_id, &mut label_scopes);

    let mut ctx = BuildContext {
        builder: &mut builder,
        exit,
        source,
        loop_stack: Vec::new(),
        cleanup_scopes: Vec::new(),
        next_scope_id: 0,
        implicit_exception_depth: 0,
        block_has_stmt: HashSet::new(),
        label_scopes,
        label_targets: HashMap::new(),
    };

    let final_flow = walk_block_stmts(&mut ctx, block, body);
    finish_flow(&mut ctx, final_flow);

    Some(builder.finish())
}

fn collect_label_scopes(
    node: Node,
    source: &[u8],
    active_scopes: &[ScopeId],
    next_scope_id: &mut ScopeId,
    labels: &mut HashMap<String, Vec<ScopeId>>,
) {
    if node.kind() == "defProc" {
        return;
    }

    if node.kind() == "label" {
        if let Some(identifier) = direct_child(node, "identifier") {
            labels.insert(
                normalize_label_name(node_text(identifier, source)),
                active_scopes.to_vec(),
            );
        }
        return;
    }

    if node.kind() == "try" && try_has_finally(node) {
        let scope_id = *next_scope_id;
        *next_scope_id += 1;
        let mut try_scopes = active_scopes.to_vec();
        try_scopes.push(scope_id);

        for child in field_children(node, "try") {
            collect_label_scopes(child, source, &try_scopes, next_scope_id, labels);
        }
        for field in ["except", "finally"] {
            for child in field_children(node, field) {
                collect_label_scopes(child, source, active_scopes, next_scope_id, labels);
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_label_scopes(child, source, active_scopes, next_scope_id, labels);
    }
}

fn try_has_finally(node: Node) -> bool {
    field_children(node, "finally")
        .iter()
        .any(|child| child.kind() == "kFinally")
}

fn normalize_label_name(name: String) -> String {
    name.to_ascii_lowercase()
}

/// Extract the procedure/function name from a `defProc` node.
///
/// For standalone procedures: `declProc > identifier`
/// For methods: `declProc > genericDot > identifier, identifier`
fn extract_proc_name(def_proc: Node, source: &[u8]) -> Option<String> {
    let decl_proc = if let Some(header) = def_proc.child_by_field_name("header") {
        header
    } else {
        let mut cursor = def_proc.walk();
        let found = def_proc
            .children(&mut cursor)
            .find(|c| c.kind() == "declProc");
        found?
    };

    // Try genericDot first (for method implementations like TClass.Method)
    let mut cursor = decl_proc.walk();
    if let Some(generic_dot) = decl_proc
        .children(&mut cursor)
        .find(|c| c.kind() == "genericDot")
    {
        let mut generic_cursor = generic_dot.walk();
        let idents: Vec<Node> = generic_dot
            .children(&mut generic_cursor)
            .filter(|c| c.kind() == "identifier")
            .collect();

        if idents.len() >= 2 {
            return Some(format!(
                "{}.{}",
                node_text(idents[0], source),
                node_text(idents[1], source)
            ));
        }
        if !idents.is_empty() {
            return Some(node_text(idents[0], source));
        }
    }

    // Try the `name` field
    if let Some(name_node) = decl_proc.child_by_field_name("name") {
        return Some(node_text(name_node, source));
    }

    // Fallback: first direct identifier child
    let mut cursor2 = decl_proc.walk();
    for child in decl_proc.children(&mut cursor2) {
        if child.kind() == "identifier" {
            return Some(node_text(child, source));
        }
    }

    None
}

/// Mutable context passed through the CFG building walk.
struct BuildContext<'a> {
    builder: &'a mut DefaultCfgBuilder,
    exit: BlockId,
    source: &'a [u8],
    loop_stack: Vec<LoopFrame>,
    cleanup_scopes: Vec<ScopeId>,
    next_scope_id: ScopeId,
    /// Nonzero while walking a try body, handler, or finally body.  This is
    /// intentionally independent from `cleanup_scopes`: handlers/finalizers
    /// may throw outward even though the scope whose handler they belong to
    /// is no longer an active catch target.
    implicit_exception_depth: usize,
    /// Protected statements are split into separate blocks so their
    /// exceptional edge cannot also cover an earlier unprotected statement.
    block_has_stmt: HashSet<BlockId>,
    /// Cleanup scopes containing each label, collected before CFG construction
    /// so forward gotos can be routed through finalizers precisely.
    label_scopes: HashMap<String, Vec<ScopeId>>,
    /// Label targets discovered while walking the procedure body.
    label_targets: HashMap<String, BlockId>,
}

fn new_block(ctx: &mut BuildContext<'_>, kind: BasicBlockKind) -> BlockId {
    ctx.builder.new_block(kind)
}

/// Add a final edge for every flow that has reached the procedure boundary.
fn finish_flow(ctx: &mut BuildContext<'_>, flow: Flow) {
    if let Some(normal) = flow.normal {
        ctx.builder.add_edge(normal, ctx.exit, EdgeKind::Normal);
    }

    for transfer in flow.transfers {
        route_transfer(ctx, transfer);
    }
}

fn route_transfer(ctx: &mut BuildContext<'_>, transfer: PendingTransfer) {
    match transfer.kind {
        TransferKind::Exception => {
            ctx.builder
                .add_edge(transfer.source, ctx.exit, EdgeKind::ExceptionThrow);
        }
        TransferKind::Exit => {
            ctx.builder.add_edge(
                transfer.source,
                ctx.exit,
                transfer_completion_edge(&transfer),
            );
        }
        TransferKind::Goto => {
            let target = transfer.target.or_else(|| {
                transfer
                    .target_label
                    .as_ref()
                    .and_then(|label| ctx.label_targets.get(label).copied())
            });
            if let Some(target) = target {
                ctx.builder
                    .add_edge(transfer.source, target, transfer_completion_edge(&transfer));
            } else {
                ctx.builder
                    .add_edge(transfer.source, ctx.exit, EdgeKind::Goto);
            }
        }
        TransferKind::Break | TransferKind::Continue => {
            if let Some(target) = transfer.target {
                ctx.builder
                    .add_edge(transfer.source, target, transfer_completion_edge(&transfer));
            }
        }
    }
}

fn transfer_completion_edge(transfer: &PendingTransfer) -> EdgeKind {
    match transfer.kind {
        TransferKind::Goto if !transfer.from_finally => EdgeKind::Goto,
        _ if transfer.from_finally => EdgeKind::FinallyExit,
        _ => EdgeKind::Normal,
    }
}

/// Walk the children of a `block`, preserving all abrupt paths from branches.
fn walk_block_stmts(ctx: &mut BuildContext<'_>, block: Node, current: BlockId) -> Flow {
    let mut cursor = block.walk();
    let mut current = Some(current);
    let mut transfers = Vec::new();

    for child in block.children(&mut cursor) {
        if matches!(
            child.kind(),
            "kBegin" | "kEnd" | ";" | "declVars" | "declConsts" | "declTypes"
        ) {
            continue;
        }

        let child_flow = process_sequence_child(ctx, child, current);
        current = child_flow.normal;
        transfers.extend(child_flow.transfers);
    }

    Flow {
        normal: current,
        transfers,
    }
}

/// Walk a `statements` node, processing each child statement.
fn walk_statements_node(
    ctx: &mut BuildContext<'_>,
    statements_node: Node,
    current: BlockId,
) -> Flow {
    let mut cursor = statements_node.walk();
    let mut current = Some(current);
    let mut transfers = Vec::new();

    for child in statements_node.children(&mut cursor) {
        if child.kind() == ";" {
            continue;
        }

        let child_flow = process_sequence_child(ctx, child, current);
        current = child_flow.normal;
        transfers.extend(child_flow.transfers);
    }

    Flow {
        normal: current,
        transfers,
    }
}

/// Process all children stored under a statement field such as `then`,
/// `else`, `body`, or an exception handler's body.
fn walk_field_children(
    ctx: &mut BuildContext<'_>,
    parent: Node,
    field_name: &str,
    current: BlockId,
    skip_k_else: bool,
) -> Flow {
    let children = field_children(parent, field_name);
    let mut current = Some(current);
    let mut transfers = Vec::new();

    for child in children {
        if child.kind() == ";" || (skip_k_else && child.kind() == "kElse") {
            continue;
        }

        let child_flow = process_sequence_child(ctx, child, current);
        current = child_flow.normal;
        transfers.extend(child_flow.transfers);
    }

    Flow {
        normal: current,
        transfers,
    }
}

/// Handle an `ifElse` node (if/then/else).
fn handle_if_else(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let condition_block = prepare_condition_block(ctx, node, current);
    let mut transfers = implicit_exception_transfers(ctx, condition_block);

    let then_block = new_block(ctx, BasicBlockKind::Normal);
    let else_block = new_block(ctx, BasicBlockKind::Normal);
    ctx.builder
        .add_edge(condition_block, then_block, EdgeKind::ConditionalTrue);
    ctx.builder
        .add_edge(condition_block, else_block, EdgeKind::ConditionalFalse);

    let then_flow = walk_field_children(ctx, node, "then", then_block, true);
    let else_flow = walk_field_children(ctx, node, "else", else_block, true);
    let normal = join_branch_flows(ctx, then_flow.normal, else_flow.normal);

    transfers.extend(then_flow.transfers);
    transfers.extend(else_flow.transfers);

    Flow { normal, transfers }
}

/// Handle an `if` node (if/then without else).
fn handle_if_only(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let condition_block = prepare_condition_block(ctx, node, current);
    let mut transfers = implicit_exception_transfers(ctx, condition_block);

    let then_block = new_block(ctx, BasicBlockKind::Normal);
    let join = new_block(ctx, BasicBlockKind::Normal);
    ctx.builder
        .add_edge(condition_block, then_block, EdgeKind::ConditionalTrue);
    ctx.builder
        .add_edge(condition_block, join, EdgeKind::ConditionalFalse);

    let then_flow = walk_field_children(ctx, node, "then", then_block, true);
    if let Some(then_end) = then_flow.normal {
        ctx.builder.add_edge(then_end, join, EdgeKind::Normal);
    }
    transfers.extend(then_flow.transfers);

    Flow {
        normal: Some(join),
        transfers,
    }
}

fn join_branch_flows(
    ctx: &mut BuildContext<'_>,
    then_end: Option<BlockId>,
    else_end: Option<BlockId>,
) -> Option<BlockId> {
    match (then_end, else_end) {
        (Some(then_end), Some(else_end)) => {
            let join = new_block(ctx, BasicBlockKind::Normal);
            ctx.builder.add_edge(then_end, join, EdgeKind::Normal);
            ctx.builder.add_edge(else_end, join, EdgeKind::Normal);
            Some(join)
        }
        (Some(end), None) | (None, Some(end)) => {
            let join = new_block(ctx, BasicBlockKind::Normal);
            ctx.builder.add_edge(end, join, EdgeKind::Normal);
            Some(join)
        }
        (None, None) => None,
    }
}

/// Handle a `for` or `while` loop.
fn handle_for_or_while(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let cond_block = new_block(ctx, BasicBlockKind::Normal);
    let body_block = new_block(ctx, BasicBlockKind::Normal);
    let after_block = new_block(ctx, BasicBlockKind::Normal);

    ctx.builder.add_edge(current, cond_block, EdgeKind::Normal);
    add_loop_header_stmt(ctx, cond_block, node);
    let mut transfers = implicit_exception_transfers(ctx, cond_block);

    ctx.builder
        .add_edge(cond_block, body_block, EdgeKind::ConditionalTrue);
    ctx.builder
        .add_edge(cond_block, after_block, EdgeKind::LoopExit);

    let target_scopes = ctx.cleanup_scopes.clone();
    ctx.loop_stack.push(LoopFrame {
        continue_target: cond_block,
        break_target: after_block,
        continue_scopes: target_scopes.clone(),
        break_scopes: target_scopes,
    });
    let body_flow = walk_field_children(ctx, node, "body", body_block, false);
    ctx.loop_stack.pop();

    if let Some(body_end) = body_flow.normal {
        ctx.builder
            .add_edge(body_end, cond_block, EdgeKind::LoopBack);
    }

    for transfer in body_flow.transfers {
        if transfer.kind == TransferKind::Break && transfer.target == Some(after_block) {
            ctx.builder.add_edge(
                transfer.source,
                after_block,
                transfer_completion_edge(&transfer),
            );
        } else if transfer.kind == TransferKind::Continue && transfer.target == Some(cond_block) {
            ctx.builder.add_edge(
                transfer.source,
                cond_block,
                transfer_completion_edge(&transfer),
            );
        } else {
            transfers.push(transfer);
        }
    }

    Flow {
        normal: Some(after_block),
        transfers,
    }
}

/// Handle a `repeat..until` loop.
fn handle_repeat(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let body_block = new_block(ctx, BasicBlockKind::Normal);
    let cond_block = new_block(ctx, BasicBlockKind::Normal);
    let after_block = new_block(ctx, BasicBlockKind::Normal);

    ctx.builder.add_edge(current, body_block, EdgeKind::Normal);

    let target_scopes = ctx.cleanup_scopes.clone();
    ctx.loop_stack.push(LoopFrame {
        continue_target: cond_block,
        break_target: after_block,
        continue_scopes: target_scopes.clone(),
        break_scopes: target_scopes,
    });
    let body_flow = walk_field_children(ctx, node, "body", body_block, false);
    ctx.loop_stack.pop();

    if let Some(body_end) = body_flow.normal {
        ctx.builder.add_edge(body_end, cond_block, EdgeKind::Normal);
    }

    let mut transfers = implicit_exception_transfers(ctx, cond_block);
    for transfer in body_flow.transfers {
        if transfer.kind == TransferKind::Break && transfer.target == Some(after_block) {
            ctx.builder.add_edge(
                transfer.source,
                after_block,
                transfer_completion_edge(&transfer),
            );
        } else if transfer.kind == TransferKind::Continue && transfer.target == Some(cond_block) {
            ctx.builder.add_edge(
                transfer.source,
                cond_block,
                transfer_completion_edge(&transfer),
            );
        } else {
            transfers.push(transfer);
        }
    }

    add_repeat_header_stmts(ctx, cond_block, node);
    ctx.builder
        .add_edge(cond_block, body_block, EdgeKind::LoopBack);
    ctx.builder
        .add_edge(cond_block, after_block, EdgeKind::LoopExit);

    Flow {
        normal: Some(after_block),
        transfers,
    }
}

/// Handle a `case..of` statement.
fn handle_case(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let selector_block = prepare_statement_block(ctx, current);
    if let Some(selector) = case_selector(node) {
        add_stmt_ref_span(
            ctx,
            selector_block,
            node.kind(),
            node.start_byte()..selector.end_byte(),
        );
    }
    let mut transfers = implicit_exception_transfers(ctx, selector_block);
    let after_block = new_block(ctx, BasicBlockKind::Normal);

    let mut cursor = node.walk();
    for arm in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "caseCase")
    {
        let arm_block = new_block(ctx, BasicBlockKind::Normal);
        ctx.builder
            .add_edge(selector_block, arm_block, EdgeKind::CaseArm);
        let arm_flow = walk_field_children(ctx, arm, "body", arm_block, false);
        if let Some(arm_end) = arm_flow.normal {
            ctx.builder.add_edge(arm_end, after_block, EdgeKind::Normal);
        }
        transfers.extend(arm_flow.transfers);
    }

    let default_children = case_default_children(node);
    if default_children.is_empty() {
        ctx.builder
            .add_edge(selector_block, after_block, EdgeKind::CaseArm);
    } else {
        let default_block = new_block(ctx, BasicBlockKind::Normal);
        ctx.builder
            .add_edge(selector_block, default_block, EdgeKind::CaseArm);
        let default_flow = walk_node_children(ctx, &default_children, default_block);
        if let Some(default_end) = default_flow.normal {
            ctx.builder
                .add_edge(default_end, after_block, EdgeKind::Normal);
        }
        transfers.extend(default_flow.transfers);
    }

    Flow {
        normal: Some(after_block),
        transfers,
    }
}

/// Handle a `with` statement.
fn handle_with(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let context_block = prepare_statement_block(ctx, current);
    if let Some(entity) = field_children(node, "entity").last() {
        add_stmt_ref_span(
            ctx,
            context_block,
            node.kind(),
            node.start_byte()..entity.end_byte(),
        );
    }
    let mut transfers = implicit_exception_transfers(ctx, context_block);

    let body_block = new_block(ctx, BasicBlockKind::Normal);
    ctx.builder
        .add_edge(context_block, body_block, EdgeKind::Normal);
    let body_flow = walk_field_children(ctx, node, "body", body_block, false);
    let after_block = new_block(ctx, BasicBlockKind::Normal);
    if let Some(body_end) = body_flow.normal {
        ctx.builder
            .add_edge(body_end, after_block, EdgeKind::Normal);
    }
    transfers.extend(body_flow.transfers);

    Flow {
        normal: Some(after_block),
        transfers,
    }
}

fn case_selector<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let selector = node.named_children(&mut cursor).find(|child| {
        !matches!(
            child.kind(),
            "caseCase" | "kCase" | "kOf" | "kElse" | "kEnd"
        )
    });
    selector
}

fn case_default_children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    let mut after_else = false;
    let mut children = Vec::new();
    for child in node.children(&mut cursor) {
        if child.kind() == "kElse" {
            after_else = true;
            continue;
        }
        if after_else && child.kind() != "kEnd" {
            children.push(child);
        }
    }
    children
}

fn walk_node_children(ctx: &mut BuildContext<'_>, children: &[Node<'_>], current: BlockId) -> Flow {
    let mut current = Some(current);
    let mut transfers = Vec::new();

    for &child in children {
        if child.kind() == ";" {
            continue;
        }
        let child_flow = process_sequence_child(ctx, child, current);
        current = child_flow.normal;
        transfers.extend(child_flow.transfers);
    }

    Flow {
        normal: current,
        transfers,
    }
}

fn process_sequence_child(
    ctx: &mut BuildContext<'_>,
    child: Node,
    current: Option<BlockId>,
) -> Flow {
    if child.is_extra() {
        return Flow {
            normal: current,
            transfers: Vec::new(),
        };
    }

    let current = current.unwrap_or_else(|| new_block(ctx, BasicBlockKind::Normal));
    if child.kind() == "label" {
        return Flow::normal(register_label(ctx, child, current));
    }

    process_single_stmt(ctx, child, current)
}

fn register_label(ctx: &mut BuildContext<'_>, label: Node, current: BlockId) -> BlockId {
    let target = if ctx.block_has_stmt.contains(&current) {
        let next = new_block(ctx, BasicBlockKind::Normal);
        ctx.builder.add_edge(current, next, EdgeKind::Normal);
        next
    } else {
        current
    };

    if let Some(identifier) = direct_child(label, "identifier") {
        let name = normalize_label_name(node_text(identifier, ctx.source));
        ctx.label_targets.insert(name, target);
    }
    add_stmt_ref(ctx, target, label);
    target
}

/// Process a single statement node in any syntactic context.
fn process_single_stmt(ctx: &mut BuildContext<'_>, child: Node, current: BlockId) -> Flow {
    if child.is_extra() {
        return Flow::normal(current);
    }

    match child.kind() {
        "block" => walk_block_stmts(ctx, child, current),
        "statements" => walk_statements_node(ctx, child, current),
        "ifElse" => handle_if_else(ctx, child, current),
        "if" => handle_if_only(ctx, child, current),
        "for" | "foreach" | "while" => handle_for_or_while(ctx, child, current),
        "case" => handle_case(ctx, child, current),
        "repeat" => handle_repeat(ctx, child, current),
        "try" => handle_try(ctx, child, current),
        "with" => handle_with(ctx, child, current),
        "raise" => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            let mut transfers = vec![raised_exception_type(child, ctx.source)
                .map(|exception_type| {
                    PendingTransfer::exception_with_type(statement_block, exception_type)
                })
                .unwrap_or_else(|| PendingTransfer::exception(statement_block))];
            if raise_may_throw_during_evaluation(child) {
                transfers.extend(implicit_exception_transfers(ctx, statement_block));
            }
            Flow {
                normal: None,
                transfers,
            }
        }
        "statement" if is_exit_call(child, ctx.source) => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            let mut transfers = vec![PendingTransfer::exit(statement_block)];
            if exit_has_argument(child, ctx.source) {
                transfers.extend(implicit_exception_transfers(ctx, statement_block));
            }
            Flow {
                normal: None,
                transfers,
            }
        }
        "statement" if is_break_call(child, ctx.source) => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            let transfer = if let Some(frame) = ctx.loop_stack.last() {
                PendingTransfer::block_target(
                    statement_block,
                    TransferKind::Break,
                    frame.break_target,
                    frame.break_scopes.clone(),
                )
            } else {
                PendingTransfer::exit(statement_block)
            };
            Flow::transfer(transfer)
        }
        "statement" if is_continue_call(child, ctx.source) => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            let transfer = if let Some(frame) = ctx.loop_stack.last() {
                PendingTransfer::block_target(
                    statement_block,
                    TransferKind::Continue,
                    frame.continue_target,
                    frame.continue_scopes.clone(),
                )
            } else {
                PendingTransfer::exit(statement_block)
            };
            Flow::transfer(transfer)
        }
        "goto" => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            let label = direct_child(child, "identifier")
                .map(|identifier| normalize_label_name(node_text(identifier, ctx.source)))
                .unwrap_or_default();
            let target_scopes = ctx.label_scopes.get(&label).cloned().unwrap_or_default();
            Flow::transfer(PendingTransfer::goto(statement_block, label, target_scopes))
        }
        _ => {
            let statement_block = prepare_statement_block(ctx, current);
            add_stmt_ref(ctx, statement_block, child);
            Flow {
                normal: Some(statement_block),
                transfers: implicit_exception_transfers(ctx, statement_block),
            }
        }
    }
}

/// Handle either `try..finally` or `try..except` based on parser fields.
fn handle_try(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let has_finally = try_has_finally(node);
    let has_except = field_children(node, "except")
        .iter()
        .any(|child| child.kind() == "kExcept");

    if has_finally {
        handle_try_finally(ctx, node, current)
    } else if has_except {
        handle_try_except(ctx, node, current)
    } else {
        // A malformed try node should not swallow the following statement.
        Flow::normal(current)
    }
}

#[derive(Debug)]
enum FinalizerInput {
    Normal(BlockId),
    Transfer(PendingTransfer),
}

/// Semantic identity of a continuation entering a finalizer. The source block
/// is deliberately absent: equivalent continuations can share one cleanup
/// body without introducing cross-path edges.
#[derive(Debug, PartialEq, Eq)]
enum FinalizerKey {
    Normal,
    Transfer {
        kind: TransferKind,
        target: Option<BlockId>,
        target_label: Option<String>,
        target_scopes: Vec<ScopeId>,
        exception_type: Option<String>,
    },
}

impl FinalizerKey {
    fn from_input(input: &FinalizerInput) -> Self {
        match input {
            FinalizerInput::Normal(_) => Self::Normal,
            FinalizerInput::Transfer(transfer) => Self::Transfer {
                kind: transfer.kind,
                target: transfer.target,
                target_label: transfer
                    .target_label
                    .as_ref()
                    .map(|label| label.to_ascii_lowercase()),
                target_scopes: transfer.target_scopes.clone(),
                exception_type: transfer
                    .exception_type
                    .as_ref()
                    .map(|exception_type| exception_type.to_ascii_lowercase()),
            },
        }
    }
}

#[derive(Debug)]
struct FinalizerGroup {
    key: FinalizerKey,
    inputs: Vec<FinalizerInput>,
}

/// Handle a `try..finally` block.
///
/// Incoming continuations share a finalizer body only when their semantic
/// pending transfer is equivalent. Distinct normal, return, loop-target, and
/// exception continuations retain separate cleanup paths.
fn handle_try_finally(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let scope_id = ctx.next_scope_id;
    ctx.next_scope_id += 1;
    let after_block = new_block(ctx, BasicBlockKind::Normal);

    ctx.cleanup_scopes.push(scope_id);
    ctx.implicit_exception_depth += 1;
    let try_flow = walk_try_body(ctx, node, current);
    ctx.implicit_exception_depth -= 1;
    ctx.cleanup_scopes.pop();

    let mut inputs = Vec::new();
    if let Some(normal) = try_flow.normal {
        inputs.push(FinalizerInput::Normal(normal));
    }
    inputs.extend(try_flow.transfers.into_iter().map(FinalizerInput::Transfer));

    let mut output = Flow::default();
    let mut groups = Vec::new();
    for input in inputs {
        if let FinalizerInput::Transfer(transfer) = &input {
            if !transfer.leaves_scope(scope_id) {
                output.transfers.push(transfer.clone());
                continue;
            }
        }

        let key = FinalizerKey::from_input(&input);
        if let Some(group) = groups
            .iter_mut()
            .find(|group: &&mut FinalizerGroup| group.key == key)
        {
            group.inputs.push(input);
        } else {
            groups.push(FinalizerGroup {
                key,
                inputs: vec![input],
            });
        }
    }

    for group in groups {
        let is_normal = matches!(group.key, FinalizerKey::Normal);
        let finally_block = new_block(ctx, BasicBlockKind::FinallyHandler);
        for input in &group.inputs {
            let (source, entry_edge) = match input {
                FinalizerInput::Normal(source) => (*source, EdgeKind::FinallyEntry),
                FinalizerInput::Transfer(transfer) => (
                    transfer.source,
                    if transfer.kind == TransferKind::Exception {
                        EdgeKind::ExceptionThrow
                    } else {
                        EdgeKind::FinallyEntry
                    },
                ),
            };
            ctx.builder.add_edge(source, finally_block, entry_edge);
        }

        ctx.implicit_exception_depth += 1;
        let finally_flow = walk_finally_body(ctx, node, finally_block);
        ctx.implicit_exception_depth -= 1;

        if let Some(finally_end) = finally_flow.normal {
            if is_normal {
                ctx.builder
                    .add_edge(finally_end, after_block, EdgeKind::FinallyExit);
                output.normal = Some(after_block);
            } else if let Some(transfer) = group.inputs.iter().find_map(|input| match input {
                FinalizerInput::Transfer(transfer) => Some(transfer),
                FinalizerInput::Normal(_) => None,
            }) {
                // The finalizer completed normally, so the original transfer
                // remains pending for outer cleanup scopes.
                output.transfers.push(transfer.with_source(finally_end));
            }
        }

        // Any transfer produced by the finalizer itself supersedes the
        // incoming transfer. Mark it as finalizer-sourced before routing it
        // through an enclosing cleanup scope (or the procedure boundary).
        output.transfers.extend(
            finally_flow
                .transfers
                .iter()
                .map(|transfer| transfer.with_source(transfer.source)),
        );
    }

    output
}

/// Handle a `try..except` block.
///
/// Typed `on` handlers are conservative alternatives because this builder has
/// no semantic exception hierarchy. A missing catch-all retains an unmatched
/// exception transfer so an enclosing handler can receive it.
fn handle_try_except(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> Flow {
    let after_block = new_block(ctx, BasicBlockKind::Normal);

    ctx.implicit_exception_depth += 1;
    let try_flow = walk_try_body(ctx, node, current);
    ctx.implicit_exception_depth -= 1;

    let mut output = Flow::default();
    if let Some(try_end) = try_flow.normal {
        ctx.builder.add_edge(try_end, after_block, EdgeKind::Normal);
        output.normal = Some(after_block);
    }

    let mut exception_sources = Vec::new();
    for transfer in try_flow.transfers {
        if transfer.kind == TransferKind::Exception {
            if let Some(existing) = exception_sources
                .iter_mut()
                .find(|existing: &&mut PendingTransfer| existing.source == transfer.source)
            {
                if existing.exception_type != transfer.exception_type {
                    existing.exception_type = None;
                }
            } else {
                exception_sources.push(transfer);
            }
        } else {
            output.transfers.push(transfer);
        }
    }

    let handlers = build_except_handlers(ctx, node, after_block);
    let has_catch_all = handlers.iter().any(|handler| handler.catch_all);

    for transfer in exception_sources {
        for handler in &handlers {
            ctx.builder
                .add_edge(transfer.source, handler.entry, EdgeKind::ExceptionThrow);
        }

        // A syntactic constructor name is not enough to prove that a handler
        // catches the exception (subclasses and qualified names need semantic
        // resolution). Keep the outward alternative unless there is a
        // catch-all handler.
        if !has_catch_all {
            output.transfers.push(transfer);
        }
    }

    for handler in handlers {
        if let Some(handler_end) = handler.flow.normal {
            ctx.builder
                .add_edge(handler_end, after_block, EdgeKind::Normal);
            output.normal = Some(after_block);
        }
        output.transfers.extend(handler.flow.transfers);
    }

    output
}

/// A handler body and its dispatch entry block.
#[derive(Debug)]
struct HandlerFlow {
    entry: BlockId,
    catch_all: bool,
    flow: Flow,
}

fn build_except_handlers(
    ctx: &mut BuildContext<'_>,
    node: Node,
    _after_block: BlockId,
) -> Vec<HandlerFlow> {
    let except_children = field_children(node, "except");
    let mut handlers = Vec::new();

    for child in except_children {
        let (entry, catch_all) = match child.kind() {
            "exceptionHandler" => (new_block(ctx, BasicBlockKind::ExceptHandler), false),
            "exceptionElse" | "statements" => {
                (new_block(ctx, BasicBlockKind::BareExceptHandler), true)
            }
            _ => continue,
        };

        ctx.implicit_exception_depth += 1;
        let flow = if child.kind() == "exceptionHandler" {
            walk_field_children(ctx, child, "body", entry, false)
        } else if child.kind() == "exceptionElse" {
            walk_exception_else_body(ctx, child, entry)
        } else {
            walk_statements_node(ctx, child, entry)
        };
        ctx.implicit_exception_depth -= 1;

        handlers.push(HandlerFlow {
            entry,
            catch_all,
            flow,
        });
    }

    if handlers.is_empty() {
        let entry = new_block(ctx, BasicBlockKind::BareExceptHandler);
        handlers.push(HandlerFlow {
            entry,
            catch_all: true,
            flow: Flow::normal(entry),
        });
    }

    handlers
}

fn walk_exception_else_body(
    ctx: &mut BuildContext<'_>,
    exception_else: Node,
    current: BlockId,
) -> Flow {
    let mut cursor = exception_else.walk();
    let mut current = Some(current);
    let mut transfers = Vec::new();

    for child in exception_else.children(&mut cursor) {
        if child.kind() == "kElse" || child.kind() == ";" {
            continue;
        }
        let child_flow = process_sequence_child(ctx, child, current);
        current = child_flow.normal;
        transfers.extend(child_flow.transfers);
    }

    Flow {
        normal: current,
        transfers,
    }
}

/// Walk the try body: the `statements` node stored in the `try` field.
fn walk_try_body(ctx: &mut BuildContext<'_>, try_node: Node, current: BlockId) -> Flow {
    let Some(body) = field_children(try_node, "try")
        .into_iter()
        .find(|child| child.kind() == "statements")
    else {
        return Flow::normal(current);
    };

    // The first protected statement must not share a block with statements
    // immediately preceding the try. This prevents a handler edge from
    // making unprotected code appear to throw into the inner handler.
    let protected_entry = new_block(ctx, BasicBlockKind::Normal);
    ctx.builder
        .add_edge(current, protected_entry, EdgeKind::Normal);
    walk_statements_node(ctx, body, protected_entry)
}

/// Walk the `statements` node after `kFinally`.
fn walk_finally_body(ctx: &mut BuildContext<'_>, try_node: Node, finally_block: BlockId) -> Flow {
    let Some(body) = field_children(try_node, "finally")
        .into_iter()
        .find(|child| child.kind() == "statements")
    else {
        return Flow::normal(finally_block);
    };

    walk_statements_node(ctx, body, finally_block)
}

fn field_children<'tree>(node: Node<'tree>, field_name: &str) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field_name, &mut cursor)
        .collect()
}

fn prepare_condition_block(ctx: &mut BuildContext<'_>, node: Node, current: BlockId) -> BlockId {
    let condition_block = prepare_statement_block(ctx, current);
    if let Some(condition) = node.child_by_field_name("condition") {
        let end = condition.end_byte();
        add_stmt_ref_span(ctx, condition_block, node.kind(), node.start_byte()..end);
    }
    condition_block
}

fn add_loop_header_stmt(ctx: &mut BuildContext<'_>, block: BlockId, node: Node) {
    let end = match node.kind() {
        "while" => node.child_by_field_name("condition"),
        "for" => node.child_by_field_name("end"),
        "foreach" => node.child_by_field_name("iterable"),
        _ => None,
    };
    if let Some(end) = end {
        add_stmt_ref_span(ctx, block, node.kind(), node.start_byte()..end.end_byte());
    }
}

fn add_repeat_header_stmts(ctx: &mut BuildContext<'_>, block: BlockId, node: Node) {
    if let Some(repeat_keyword) = direct_child(node, "kRepeat") {
        add_stmt_ref_span(
            ctx,
            block,
            node.kind(),
            repeat_keyword.start_byte()..repeat_keyword.end_byte(),
        );
    }
    if let Some(condition) = node.child_by_field_name("condition") {
        add_stmt_ref(ctx, block, condition);
    }
}

fn direct_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let child = node
        .children(&mut cursor)
        .find(|child| child.kind() == kind);
    child
}

/// Use a fresh protected block after an existing statement. Unprotected
/// straight-line code remains coalesced into the historical body block.
fn prepare_statement_block(ctx: &mut BuildContext<'_>, current: BlockId) -> BlockId {
    if ctx.implicit_exception_depth == 0 || !ctx.block_has_stmt.contains(&current) {
        return current;
    }

    let next = new_block(ctx, BasicBlockKind::Normal);
    ctx.builder.add_edge(current, next, EdgeKind::Normal);
    next
}

fn implicit_exception_transfers(ctx: &BuildContext<'_>, source: BlockId) -> Vec<PendingTransfer> {
    if ctx.implicit_exception_depth == 0 {
        Vec::new()
    } else {
        vec![PendingTransfer::exception(source)]
    }
}

/// Add a source-level statement reference and remember that the block is no
/// longer safe to reuse for another protected statement.
fn add_stmt_ref(ctx: &mut BuildContext<'_>, block: BlockId, node: Node) {
    add_stmt_ref_span(ctx, block, node.kind(), node.start_byte()..node.end_byte());
}

fn add_stmt_ref_span(
    ctx: &mut BuildContext<'_>,
    block: BlockId,
    node_kind: &str,
    byte_range: Range<usize>,
) {
    ctx.builder.add_stmt(
        block,
        StmtRef {
            byte_range,
            node_kind: node_kind.to_string(),
        },
    );
    ctx.block_has_stmt.insert(block);
}
