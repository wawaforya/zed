# Vendored httpyac grammar

Source: the private `tree-sitter-httpyac` repository, revision
`a20a7de419d648d1c92e9a323b13abb37da8735d` (MIT; see LICENSE).
The HTTP language queries/tasks and snippets come from `zed-httpyac`, revision
`5f5ae41c553e43f8b9bcf5a454317a1307dcfcae` (MIT).

`src/parser.c` is checked in so building Zed does not require Node or the
Tree-sitter CLI. Regenerate it from `grammar.js` with Tree-sitter CLI 0.26.9
when updating the grammar, and run `tree-sitter test` in this directory.
Zed compiles the parser only with the `load-grammars` feature.
