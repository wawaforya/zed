unsafe extern "C" {
    fn tree_sitter_httpyac() -> *const ();
}

pub(super) const LANGUAGE: tree_sitter_language::LanguageFn =
    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_httpyac) };

#[cfg(test)]
mod tests {
    #[test]
    fn http_queries_compile() {
        let language = super::LANGUAGE.into();
        for path in super::super::GrammarDir::iter() {
            if path.starts_with("http/") && path.ends_with(".scm") {
                let file = super::super::GrammarDir::get(&path).expect("HTTP query exists");
                let source = std::str::from_utf8(&file.data).expect("HTTP query is UTF-8");
                tree_sitter::Query::new(&language, source)
                    .unwrap_or_else(|error| panic!("invalid query {path}: {error}"));
            }
        }
    }

    #[test]
    fn http_requests_parse() {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&super::LANGUAGE.into()).unwrap();
        let tree = parser
            .parse(
                "# @name login\nPOST https://example.com/login\nContent-Type: application/json\n\n{\"user\":\"test\"}\n\n###\n# @ref login\nGET https://example.com/me\n",
                None,
            )
            .unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}
