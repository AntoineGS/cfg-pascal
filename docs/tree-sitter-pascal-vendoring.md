# Vendored Pascal parser

`vendor/tree-sitter-pascal` is the minimal Rust package payload from the
crates.io `tree-sitter-pascal` 0.10.2 release. Its parser, grammar JSON,
node-types JSON, Rust binding, build script, queries, README, and MIT license
were copied from the local Cargo registry source directory:

```
/home/antoinegs/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tree-sitter-pascal-0.10.2
```

The payload matches the v0.10.2 source at
`https://github.com/Isopod/tree-sitter-pascal` (source commit
`042119eca2e18a60e56317fb06ee3ba5c32cb447`, grammar commit history; the
registry package records git source `2f28b717be47cf592241e1b7bec3b2b906f59148`).
The original upstream `LICENSE` is retained in the vendored package. The
original grammar corpus was copied from the exact `v0.10.2` tag and lives under
`vendor/tree-sitter-pascal/test/corpus`.

The crate dependency is a repository-relative path, so consumers do not need
the developer's Cargo registry path. `cfg_pascal::LANGUAGE` is the language
function for this patched parser. Consumers that previously obtained
`tree_sitter_pascal::LANGUAGE` from a separately selected or unpatched crate
should use the re-export instead; existing `build_file_cfgs` callers and
already-parsed `tree_sitter::Tree` values remain source-compatible.

## Local grammar changes

The vendored grammar starts at upstream 0.10.2. Task 1 adds only:

* decimal-only label tokens for declarations, definitions, and `goto`;
* the legacy unit `begin..end.` initialization form;
* an optional expression on `raise`, allowing `raise;` to parse.

Generated `src/parser.c`, `src/grammar.json`, and `src/node-types.json` must
always be regenerated together with the pinned CLI. Do not edit those files by
hand.

## Regeneration and corpus tests

The pinned tool lock is `tools/tree-sitter-cli/package-lock.json`. From the
repository root, install the ignored tool directory and run the CLI:

```sh
npm ci --prefix tools/tree-sitter-cli
cd vendor/tree-sitter-pascal
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter generate
../../tools/tree-sitter-cli/node_modules/.bin/tree-sitter test
```

The last two commands must be run from `vendor/tree-sitter-pascal`, or with the
vendored directory supplied as the command's working directory. The expected
CLI version is `tree-sitter 0.24.7`; `npm ci` creates only ignored
`node_modules` files. In environments where npm is unavailable, the equivalent
reproducible check is `cargo install --locked --version 0.24.7 tree-sitter-cli`
followed by the same `generate` and `test` commands. The committed generated
artifacts were produced and verified with CLI 0.24.7.
