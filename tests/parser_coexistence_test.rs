use cfg_core::EdgeKind;
use tree_sitter::{Language, Parser, Tree};

const SHARED_MODERN_SOURCE: &[u8] = br#"
unit SharedModern;
interface
type
  TBox<T> = class
    procedure Run(Value: T);
  end;
implementation
procedure TBox<T>.Run(Value: T);
begin
  for var Index := 0 to 2 do
  begin
    if Value is not TWidget then
      Visit(Value, Index);
  end;
end;
end.
"#;

fn parse(language: Language, source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("Pascal language must initialise");
    parser
        .parse(source, None)
        .expect("parser must return a tree")
}

fn parse_clean(language: Language, source: &[u8]) -> Tree {
    let tree = parse(language, source);
    assert!(
        !tree.root_node().has_error(),
        "source must parse cleanly:\n{}",
        tree.root_node().to_sexp()
    );
    tree
}

#[test]
fn local_and_upstream_languages_link_distinctly_and_share_modern_cfg_paths() {
    let local_language: Language = cfg_pascal::LANGUAGE.into();
    let upstream_language: Language = lsp_tree_sitter_pascal::LANGUAGE.into();
    assert_ne!(
        local_language, upstream_language,
        "the local and upstream language functions must resolve to distinct grammars"
    );

    let local_tree = parse_clean(local_language, SHARED_MODERN_SOURCE);
    let upstream_tree = parse_clean(upstream_language, SHARED_MODERN_SOURCE);
    assert_eq!(
        local_tree.root_node().to_sexp(),
        upstream_tree.root_node().to_sexp(),
        "local and upstream grammars must agree on shared modern syntax"
    );

    let local_cfgs = cfg_pascal::build_file_cfgs(&local_tree, SHARED_MODERN_SOURCE);
    let upstream_cfgs = cfg_pascal::build_file_cfgs(&upstream_tree, SHARED_MODERN_SOURCE);
    for cfgs in [&local_cfgs, &upstream_cfgs] {
        let cfg = cfgs
            .iter()
            .find(|cfg| {
                cfg.graph
                    .edge_indices()
                    .any(|edge| cfg.graph[edge] == EdgeKind::LoopBack)
            })
            .unwrap_or_else(|| {
                let names: Vec<&str> = cfgs.iter().map(|cfg| cfg.proc_name.as_str()).collect();
                panic!("shared modern procedure must produce a loop CFG; found {names:?}")
            });
        assert!(cfg.is_reachable(cfg.entry, cfg.exit));
    }
}

#[test]
fn local_patched_syntax_is_not_accidentally_bound_to_upstream_parser() {
    for (description, source) in [
        (
            "numeric labels",
            b"program LabelOnly; label 1; begin if Cond then 1: Work; end.".as_slice(),
        ),
        (
            "legacy unit body",
            b"unit LegacyUnit; interface implementation begin LegacyInit; end.".as_slice(),
        ),
    ] {
        let local_tree = parse_clean(cfg_pascal::LANGUAGE.into(), source);
        let upstream_tree = parse(lsp_tree_sitter_pascal::LANGUAGE.into(), source);
        assert!(
            upstream_tree.root_node().has_error(),
            "upstream parser unexpectedly accepted patched-only {description}:\n{}",
            upstream_tree.root_node().to_sexp()
        );
        assert!(
            !local_tree.root_node().has_error(),
            "local parser must retain patched-only {description} support"
        );
    }
}
