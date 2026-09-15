# CFG and LSP alignment boundary

`cfg-pascal` builds a control-flow graph from one parsed Pascal syntax tree. It
does not provide project indexing, conditional-build evaluation, receiver
resolution, or an LSP server. Syntax recognition and CFG semantic precision
are therefore documented separately below.

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

## Exception precision boundary

Typed constructor dispatch is precise only for proven same-file, non-generic
classes and transparent aliases. Missing/imported types, conditional or
malformed declarations, unresolved method owners, generic forms, implicit
`with` members, and unsupported expression/type shapes retain conservative
alternatives.

## Remaining LSP integration work

These items belong to the existing LSP/lint integration rather than this
single-file CFG crate:

- a shared-tree lint entrypoint and context adapter;
- receiver resolution for method and property references;
- project conditional and MSBuild configuration fidelity; and
- semantic-token integration.

No duplicate language server or project index is introduced here.
