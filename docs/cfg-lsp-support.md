# CFG and LSP alignment boundary

`cfg-pascal` builds a control-flow graph from a parsed Pascal syntax tree. The
legacy `build_file_cfgs` API is file-local; the additive
`build_file_cfgs_in_project` API consumes an immutable caller-built project
snapshot. This crate still does not provide project indexing,
conditional-build evaluation, receiver resolution, or an LSP server. Syntax
recognition and CFG semantic precision are therefore documented separately
below.

## Current syntax and CFG semantics

| Syntax | Parser support | CFG support and limits |
| --- | --- | --- |
| Modern `ppBlock` statement conditionals | Yes | Branches are mutually exclusive alternatives. Every branch is possible because this crate does not evaluate project defines. A block without `else` retains a skip path. Nested blocks, loop transfers, labels, and `finally` cleanup are preserved. Directives are not executable statement references. |
| Preprocessor-wrapped declarations | Yes | Exception type inference treats modern `pp*` nodes as a file-wide uncertainty barrier. A conditional type or alias never removes a conservative exception edge. Ordinary comments are not barriers. |
| `case ... otherwise` / `case ... else` | Yes | The default arm is a `CaseArm` alternative. An additional no-match path is emitted only when no default arm exists. |
| Inline variables | Yes | Inline value bindings shadow types from their declaration onward. `for var` and `foreach var` bindings are scoped to the loop syntax and do not leak into later code. |
| Anonymous functions/procedures | Yes | Lambda arguments and local declarations have a separate lexical scope for semantic lookup. Anonymous bodies are not walked as statements in the enclosing routine and do not produce a separate public CFG name. |
| Delphi conditional expressions (`if ... then ... else ...`) | Yes | The enclosing expression-bearing statement is currently atomic in the CFG. The graph does not create branch blocks or claim that both expression arms execute; exception behavior is conservatively attributed to the containing statement. Expression-level side effects and branch-specific source references are not modeled yet. |
| Malformed or incremental trees | Tree-sitter can represent them | Callers must pass a syntax tree and source bytes from the same snapshot. The builder uses conservative fallbacks for unsupported or incomplete nodes; it does not fabricate compiler semantics. |

## Project snapshot API

Project-aware builds never discover files or infer imports from spelling. The
caller creates a `ProjectSnapshot` from `ProjectUnitInput` values (each pairing
a parsed `tree_sitter::Tree` with its source bytes) and supplies one
`ImportBinding` for each `uses`-clause `moduleName` occurrence. A binding can
select a loaded `ProjectUnitId`, or explicitly record an unavailable or
ambiguous target. Authorized qualifiers are occurrence-specific, so aliases
must be supplied by the project model rather than guessed by this crate.

The snapshot validates duplicate unit/source IDs, exact `uses`-site spans,
duplicate bindings, dangling loaded targets, qualifier syntax, and root/source
byte bounds. The tree/source pairing remains a caller contract because
tree-sitter does not expose the bytes originally used to create a `Tree`.
Snapshot fields are private and the builder only borrows them, so a completed
snapshot cannot be mutated through the resolver.

```rust
use cfg_pascal::{
    build_file_cfgs_in_project, ProjectSnapshot, ProjectSourceId, ProjectUnitId,
    ProjectUnitInput,
};
use tree_sitter::Parser;

let source = b"unit Demo; interface implementation end.";
let mut parser = Parser::new();
parser.set_language(&cfg_pascal::LANGUAGE.into()).unwrap();
let tree = parser.parse(source, None).unwrap();
let unit = ProjectUnitInput::new(
    ProjectUnitId::from("demo"),
    ProjectSourceId::from("demo.pas"),
    tree,
    source,
);
let snapshot = ProjectSnapshot::new(vec![unit], Vec::new()).unwrap();
let cfgs = build_file_cfgs_in_project(&snapshot, &ProjectUnitId::from("demo")).unwrap();
assert!(cfgs.is_empty());
```

## Exception precision boundary

Typed constructor dispatch is precise only for proven non-generic classes and
transparent aliases. The legacy API proves same-file declarations; the project
API can additionally prove declarations in explicitly loaded units through
explicit uses bindings. Missing, unavailable, ambiguous, or unimported types,
conditional or malformed declarations, unresolved method owners, generic
forms, inaccessible constructor members, implicit `with` members, and
unsupported expression/type shapes retain conservative alternatives.

Project import lookup uses interface declarations as the exported namespace,
honors interface versus implementation uses sections and later-entry
precedence, and treats an unresolved higher-priority import as a blocker. Unit
identities are caller-supplied; type identity is qualified by the stable unit
identity and does not depend on input order. Cyclic ancestry and alias graphs
remain unknown.

## Remaining LSP integration work

These items belong to the existing LSP/lint integration rather than this
single-file CFG crate:

- a shared-tree lint entrypoint and context adapter;
- project-index/configuration adapters that construct `ProjectSnapshot` values;
- receiver resolution for method and property references;
- project conditional and MSBuild configuration fidelity; and
- semantic-token integration.

No duplicate language server or project index is introduced here.
