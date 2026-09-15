# Vendored Pascal parser

`vendor/tree-sitter-pascal` is the minimal Rust package payload from the
MIT-licensed `AntoineGS/tree-sitter-pascal` 0.11.0 source at commit
`2f95d9cd6af861b364dd73b7a99529a2c08300af`. Its parser, grammar JSON,
node-types JSON, external scanner, Rust binding, build script, queries, README,
and MIT license are copied from the exact tracked source at that commit:

```
https://github.com/AntoineGS/tree-sitter-pascal/tree/2f95d9cd6af861b364dd73b7a99529a2c08300af
```

The original upstream `LICENSE` is retained in the vendored package. The
upstream corpus is merged with cfg-pascal's regression corpus under
`vendor/tree-sitter-pascal/test/corpus`; the upstream files are copied from the
same commit rather than from an untracked sibling worktree.

The crate dependency is a repository-relative path, so consumers do not need
the developer's Cargo registry path. `cfg_pascal::LANGUAGE` is the language
function for this patched parser. Consumers that previously obtained
`tree_sitter_pascal::LANGUAGE` from a separately selected or unpatched crate
should use the re-export instead; existing `build_file_cfgs` callers and
already-parsed `tree_sitter::Tree` values remain source-compatible.

## Local grammar changes

The vendored grammar starts at upstream 0.11.0. Task 1 adds only:

* decimal-only label tokens for declarations, definitions, and `goto`;
* labeled single-statement prefixes, including named and numeric labels;
* trailing-statement label separation so missing separators remain errors;
* the legacy unit `begin..end.` initialization form;
* the existing CFG-compatible AST aliases and field shapes where upstream
  retained them. Bare `raise;` is already provided by upstream 0.11.0.

Generated `src/parser.c`, `src/grammar.json`, and `src/node-types.json` must
always be regenerated together with the pinned CLI. The external scanner is
compiled by `bindings/rust/build.rs`; do not edit generated artifacts by hand.

## Regeneration and corpus tests

The pinned tool lock is `tools/tree-sitter-cli/package-lock.json`. From the
repository root, install the ignored tool directory and run the CLI:

```sh
npm ci --prefix tools/tree-sitter-cli
cd vendor/tree-sitter-pascal
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter generate
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter test
```

With npm 12, the first install may block the pinned CLI's native install
script. Approve that exact package from the tool directory, then rebuild it:

```sh
cd tools/tree-sitter-cli
npm install-scripts approve tree-sitter-cli
npm rebuild tree-sitter-cli
cd ../../vendor/tree-sitter-pascal
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter generate
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter test
```

The approval is recorded in `tools/tree-sitter-cli/package.json` under
`allowScripts`; `node_modules` is intentionally ignored. If npm is not
available, install the same pinned CLI with Cargo instead:

```sh
cargo install --locked --version 0.24.7 tree-sitter-cli
cd vendor/tree-sitter-pascal
tree-sitter generate
tree-sitter test
```

The `generate` and `test` commands must be run from
`vendor/tree-sitter-pascal`, or with the vendored directory supplied as the
command's working directory. The expected CLI version is `tree-sitter 0.24.7`;
`npm ci` creates only ignored `node_modules` files. The committed generated
artifacts were produced and verified with CLI 0.24.7.
