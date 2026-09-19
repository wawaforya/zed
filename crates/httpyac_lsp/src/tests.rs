fn position(line: u32, character: u32) -> crate::index::Position {
    crate::index::Position { line, character }
}

fn document(text: &str) -> crate::index::Document {
    crate::index::Document::parse("untitled:test.http".into(), text.into())
}

#[test]
fn utf16_and_crlf_offsets() {
    assert_eq!(crate::index::offset("中😀x\r\n后", position(0, 3)), Some(7));
    assert_eq!(crate::index::offset("中😀x\r\n后", position(0, 2)), None);
    assert_eq!(
        crate::index::offset("中😀x\r\n后", position(1, 1)),
        Some(13)
    );
    let indexed = document("@token = 1\r\nGET https://example.invalid/😀/{{token.value}}");
    let reference = &indexed.variable_references[0];
    assert_eq!(reference.range.start.character, 33);
    assert_eq!(
        crate::index::offset(&indexed.text, reference.range.start)
            .map(|start| &indexed.text[start..start + 5]),
        Some("token")
    );
}

#[test]
fn indexes_metadata_scripts_paths_and_json_comments() {
    let indexed = document(
        "@baseUrl = https://example.invalid\n// @import=./shared.http\n// @name=My Login\nPOST {{baseUrl}}/login\nContent-Type: application/json\n\n{\n // note\n \"value\": 1,\n /*\n comment\n */\n \"next\": 2\n}\n\n> {%\nclient.global.set('token', response.body.token);\n%}\n###\n// @ref=My Login\nGET {{baseUrl}}/me\nAuthorization: Bearer {{MyLogin.token}}\n",
    );
    assert_eq!(indexed.requests.len(), 2);
    assert_eq!(indexed.requests[0].response_name, "MyLogin");
    assert_eq!(indexed.request_references[0].name, "My Login");
    assert_eq!(indexed.paths[0].kind, "import");
    assert_eq!(indexed.json_bodies.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&indexed.json_bodies[0].1).unwrap();
    assert_eq!(body["next"], 2);
    assert!(indexed.protected.contains(&16));
    assert!(indexed.runtime_names.contains("token"));
    let indexed = document("{{\nGET https://must-not-run.invalid\n}}\nGET https://example.invalid");
    assert_eq!(indexed.requests.len(), 1);
}

#[test]
fn formatting_never_changes_body_or_scripts() {
    let indexed = document(
        "@base = url   \r\nPOST https://example.invalid   \r\nContent-Type: application/json\r\n\r\n{\r\n \"a\": 1   \r\n}   \r\n> {%\r\n script();   \r\n%}\r\n",
    );
    let edits = crate::features::formatting(&indexed);
    assert_eq!(edits.len(), 2);
    assert!(
        edits
            .iter()
            .all(|edit| edit["range"]["start"]["line"].as_u64().unwrap() <= 1)
    );
}

#[test]
fn imported_symbols_rename_by_definition_not_name() {
    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join("shared.http");
    let other = directory.path().join("other.http");
    let main = directory.path().join("main.http");
    std::fs::write(&shared, "# @name My Login\nGET https://example.invalid\n").unwrap();
    std::fs::write(&other, "# @name My Login\nGET https://unrelated.invalid\n").unwrap();
    let main_uri = crate::workspace::file_uri(&main).unwrap();
    let shared_uri = crate::workspace::file_uri(&shared).unwrap();
    let other_uri = crate::workspace::file_uri(&other).unwrap();
    let mut workspace = crate::workspace::Workspace::default();
    workspace
        .roots
        .insert(crate::workspace::file_uri(directory.path()).unwrap());
    workspace.update(
        &main_uri,
        "# @import ./shared.http\n# @ref My Login\nGET https://example.invalid/{{MyLogin.token}}"
            .into(),
        1,
    );
    workspace.refresh();
    assert!(workspace.affected(&shared_uri).contains(&main_uri));
    let references = crate::features::references(&workspace, &shared_uri, position(0, 10), true);
    assert_eq!(references.len(), 3);
    assert!(
        references
            .iter()
            .all(|reference| reference["uri"] != other_uri)
    );
    let edit = workspace
        .rename(&shared_uri, position(0, 10), "New Session")
        .unwrap();
    let edits = edit["documentChanges"].as_array().unwrap();
    assert_eq!(edits.len(), 2);
    assert!(
        edits
            .iter()
            .all(|edit| edit["textDocument"]["uri"] != other_uri)
    );
    assert!(
        edits
            .iter()
            .flat_map(|edit| edit["edits"].as_array().unwrap())
            .any(|edit| edit["newText"] == "NewSession")
    );
    workspace.update(&main_uri, "# @import ./shared.http\n# @import ./other.http\n# @ref My Login\nGET https://example.invalid".into(), 2);
    workspace.refresh();
    assert!(
        workspace
            .rename(&shared_uri, position(0, 10), "New Session")
            .is_err()
    );
}

#[test]
fn rename_rejects_collisions_and_keeps_dotted_suffix() {
    let mut workspace = crate::workspace::Workspace::default();
    let uri = "untitled:variables.http";
    workspace.update(
        uri,
        "@token = x\n@other = y\nGET https://example.invalid/{{token.value}}".into(),
        1,
    );
    workspace.refresh();
    assert!(workspace.rename(uri, position(0, 3), "other").is_err());
    assert_eq!(
        workspace.at(uri, position(2, 40)).unwrap().candidates.len(),
        1
    );
    let edit = workspace.rename(uri, position(0, 3), "session").unwrap();
    let edits = edit["documentChanges"][0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 2);
    let reference = &edits[1]["range"];
    assert_eq!(
        reference["end"]["character"].as_u64().unwrap()
            - reference["start"]["character"].as_u64().unwrap(),
        5
    );
}

#[test]
fn disk_refresh_preserves_open_buffers_and_reloads_closed_files() {
    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join("shared.http");
    let main = directory.path().join("main.http");
    std::fs::write(&shared, "# @name disk\nGET https://example.invalid").unwrap();
    let shared_uri = crate::workspace::file_uri(&shared).unwrap();
    let main_uri = crate::workspace::file_uri(&main).unwrap();
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update(
        &main_uri,
        "# @import ./shared.http\n# @ref edited\nGET https://example.invalid".into(),
        1,
    );
    workspace.update(
        &shared_uri,
        "# @name edited\nGET https://example.invalid".into(),
        1,
    );
    workspace.refresh();
    assert_eq!(workspace.definitions(&main_uri, "edited", true).len(), 1);
    workspace.close(&shared_uri);
    workspace.refresh();
    assert!(workspace.definitions(&main_uri, "edited", true).is_empty());
    assert_eq!(workspace.definitions(&main_uri, "disk", true).len(), 1);
    std::fs::remove_file(shared).unwrap();
    workspace.refresh();
    assert!(workspace.definitions(&main_uri, "disk", true).is_empty());
}

#[test]
fn cycles_diagnostics_and_dynamic_variables_are_static() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.http");
    let uri = crate::workspace::file_uri(&path).unwrap();
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update(&uri, "# @import ./main.http\n# @name first\n# @ref second\n# @timeout eventually\n# @loop around forever\n# @ratelimit max many\nFETCH https://example.invalid/{{fromEnvironment}}\nContent-Type: application/json\n\n{\"bad\":}\n###\n# @name second\n# @ref first\nGET https://example.invalid\n".into(), 1);
    workspace.refresh();
    let diagnostics = crate::features::diagnostics(
        &workspace,
        &workspace.documents[&uri],
        &crate::features::Catalog::load().unwrap(),
    );
    for code in [
        "import-cycle",
        "reference-cycle",
        "invalid-method",
        "invalid-json",
        "invalid-metadata-value",
        "unresolved-variable",
    ] {
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["code"] == code),
            "missing {code}: {diagnostics:?}"
        );
    }
    assert!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "unresolved-variable")
            .all(|diagnostic| diagnostic["severity"] == 4)
    );
}

#[test]
fn json_body_accepts_only_httpyac_comments() {
    for body in ["{\n \"x\": 1 // inline\n}", "{\n \"x\": 1,\n}"] {
        let mut workspace = crate::workspace::Workspace::default();
        workspace.update(
            "untitled:json",
            format!("POST https://example.invalid\nContent-Type: application/json\n\n{body}"),
            1,
        );
        workspace.refresh();
        assert!(
            crate::features::diagnostics(
                &workspace,
                &workspace.documents["untitled:json"],
                &crate::features::Catalog::load().unwrap()
            )
            .iter()
            .any(|diagnostic| diagnostic["code"] == "invalid-json")
        );
    }
}

#[test]
fn completions_hover_links_actions_and_symbols() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.http");
    let uri = crate::workspace::file_uri(&path).unwrap();
    std::fs::write(
        directory.path().join("shared.http"),
        "# @name shared\nGET https://example.invalid",
    )
    .unwrap();
    let catalog = crate::features::Catalog::load().unwrap();
    for (text, line, character, expected) in [
        ("", 0, 0, "GET"),
        ("# @", 0, 3, "@name"),
        ("@base = url\nGET {{", 1, 6, "base"),
        ("GET https://example.invalid\nAuth", 1, 4, "Authorization"),
        ("# @import ./sh", 0, 13, "shared.http"),
    ] {
        let mut workspace = crate::workspace::Workspace::default();
        workspace.update(&uri, text.into(), 1);
        workspace.refresh();
        let completions = crate::features::completions(
            &workspace,
            &workspace.documents[&uri],
            position(line, character),
            &catalog,
        );
        assert!(
            completions.iter().any(|item| item["label"] == expected),
            "{expected}: {completions:?}"
        );
    }
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update(&uri, "@baseUrl = url\n# @import ./shared.http\n# @ref share\nGET {{baseUrll}}\nAuthorization: Bearer {{$timestamp}}\n".into(), 1);
    workspace.refresh();
    let indexed = &workspace.documents[&uri];
    assert_eq!(crate::features::links(indexed).len(), 1);
    assert_eq!(crate::features::symbols(&workspace, "shar").len(), 1);
    assert!(
        crate::features::hover(&workspace, indexed, position(4, 4), &catalog)
            .to_string()
            .contains("Authentication credentials")
    );
    let diagnostics = crate::features::diagnostics(&workspace, indexed, &catalog);
    let actions = crate::features::actions(
        &workspace,
        indexed,
        &diagnostics,
        crate::index::Range {
            start: position(3, 0),
            end: position(3, 15),
        },
    );
    assert_eq!(actions.len(), 4, "{actions:?}");
}

#[test]
fn protocol_lifecycle_incremental_edits_and_versions() {
    let mut server = super::Server::new().unwrap();
    let mut output = Vec::new();
    server
        .handle(
            serde_json::json!({"id": 1, "method": "initialize", "params": {"capabilities": {}}}),
            &mut output,
        )
        .unwrap();
    let uri = "untitled:protocol";
    server.handle(serde_json::json!({"method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "version": 1, "text": "@token = 😀\r\nGET {{token}}"}}}), &mut output).unwrap();
    server.handle(serde_json::json!({"method": "textDocument/didChange", "params": {"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"range": {"start": {"line": 0, "character": 9}, "end": {"line": 0, "character": 11}}, "text": "中"}]}}), &mut output).unwrap();
    assert!(
        server.workspace.documents[uri]
            .text
            .starts_with("@token = 中")
    );
    server.handle(serde_json::json!({"method": "textDocument/didChange", "params": {"textDocument": {"uri": uri, "version": 1}, "contentChanges": [{"text": "stale"}]}}), &mut output).unwrap();
    assert_ne!(server.workspace.documents[uri].text, "stale");
    server
        .handle(
            serde_json::json!({"id": 2, "method": "shutdown"}),
            &mut output,
        )
        .unwrap();
    assert!(
        server
            .handle(serde_json::json!({"method": "exit"}), &mut output)
            .unwrap()
    );
    let mut input = std::io::Cursor::new(output);
    assert_eq!(
        super::read_message(&mut input).unwrap().unwrap()["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
}

#[test]
fn protocol_folder_removal_preserves_imported_files() {
    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join("shared.http");
    std::fs::write(&shared, "# @name shared\nGET https://example.invalid").unwrap();
    let root = crate::workspace::file_uri(directory.path()).unwrap();
    let shared_uri = crate::workspace::file_uri(&shared).unwrap();
    let mut server = super::Server::new().unwrap();
    let mut output = Vec::new();
    server.handle(serde_json::json!({"id": 1, "method": "initialize", "params": {"workspaceFolders": [{"uri": root, "name": "test"}]}}), &mut output).unwrap();
    server
        .handle(
            serde_json::json!({"method": "initialized", "params": {}}),
            &mut output,
        )
        .unwrap();
    assert!(server.workspace.documents.contains_key(&shared_uri));
    server.handle(serde_json::json!({"method": "workspace/didChangeWorkspaceFolders", "params": {"event": {"removed": [{"uri": root}], "added": []}}}), &mut output).unwrap();
    assert!(server.workspace.documents.is_empty());
    let main_uri = crate::workspace::file_uri(&directory.path().join("main.http")).unwrap();
    server.workspace.update(
        &main_uri,
        "# @import ./shared.http\nGET https://example.invalid".into(),
        1,
    );
    server.workspace.refresh();
    assert!(server.workspace.documents.contains_key(&shared_uri));
}

#[test]
fn path_uri_round_trip_and_relative_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("中 space#%.http");
    let uri = crate::workspace::file_uri(&path).unwrap();
    assert_eq!(url::Url::parse(&uri).unwrap().to_file_path().unwrap(), path);
    assert_eq!(
        crate::workspace::resolve_path(&uri, "./child/../other.http").unwrap(),
        directory.path().join("other.http")
    );
    #[cfg(windows)]
    {
        let uri =
            crate::workspace::file_uri(std::path::Path::new(r"\\server\share\中文 file.http"))
                .unwrap();
        assert_eq!(
            crate::workspace::resolve_path(&uri, r".\child.http").unwrap(),
            std::path::PathBuf::from(r"\\server\share\child.http")
        );
    }
}

#[test]
fn incomplete_documents_are_safe_during_typing() {
    let catalog = crate::features::Catalog::load().unwrap();
    let lines = [
        "",
        "#",
        "# @import '",
        "# @proto \"",
        "< '",
        "> \"",
        "base =",
        "@base :=",
        "GET https://example.invalid",
        "Content-Type: application/json",
        "{",
        "}",
        "[",
        "]",
        "/*",
        "*/",
        "//",
        "{{",
        "}}",
        "> {%",
        "%}",
        "###",
        "# @name",
        "# @ref",
        "😀{{base",
        "\"value\":",
        "?? status ==",
    ];
    for first in lines {
        for second in lines {
            let mut workspace = crate::workspace::Workspace::default();
            workspace.update("untitled:incomplete", format!("{first}\n{second}"), 1);
            workspace.refresh();
            let indexed = &workspace.documents["untitled:incomplete"];
            let diagnostics = crate::features::diagnostics(&workspace, indexed, &catalog);
            crate::features::formatting(indexed);
            crate::features::actions(
                &workspace,
                indexed,
                &diagnostics,
                crate::index::Range::default(),
            );
            crate::features::completions(
                &workspace,
                indexed,
                position(1, second.encode_utf16().count() as u32),
                &catalog,
            );
        }
    }
    assert_eq!(document("base =").variables.len(), 1);
}

#[test]
fn runtime_exports_do_not_disable_unrelated_variable_checks() {
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update("untitled:runtime", "{{\nclient.global.set('token', 'secret');\nexports.session = {};\n}}\nGET https://example.invalid\nAuthorization: {{token}}\nX-Session: {{session.id}}\nX-Unknown: {{unknown}}".into(), 1);
    workspace.refresh();
    let diagnostics = crate::features::diagnostics(
        &workspace,
        &workspace.documents["untitled:runtime"],
        &crate::features::Catalog::load().unwrap(),
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "unresolved-variable");
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 7);
}

#[test]
fn rename_rejects_normalized_response_collision() {
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update("untitled:collision", "# @name login\nGET https://example.invalid\n###\n# @name New Session\nGET https://example.invalid".into(), 1);
    workspace.refresh();
    assert!(
        workspace
            .rename("untitled:collision", position(0, 10), "NewSession")
            .is_err()
    );
    assert!(
        workspace
            .rename("untitled:collision", position(0, 10), "bad#name")
            .is_err()
    );
}

#[test]
fn malformed_changes_are_atomic_and_multiple_changes_are_sequential() {
    let mut server = super::Server::new().unwrap();
    let mut output = Vec::new();
    server
        .handle(
            serde_json::json!({"id": 1, "method": "initialize", "params": {}}),
            &mut output,
        )
        .unwrap();
    server.workspace.update("untitled:edits", "😀x".into(), 1);
    let change = |version, changes| serde_json::json!({"method": "textDocument/didChange", "params": {"textDocument": {"uri": "untitled:edits", "version": version}, "contentChanges": changes}});
    server.handle(change(2, serde_json::json!([
        {"range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 3}}, "text": "y"},
        {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 2}}, "text": "invalid"}
    ])), &mut output).unwrap();
    assert_eq!(server.workspace.documents["untitled:edits"].text, "😀x");
    assert_eq!(server.workspace.open["untitled:edits"], 1);
    server.handle(change(3, serde_json::json!([
        {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}}, "text": "中"},
        {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 2}}, "text": "y"}
    ])), &mut output).unwrap();
    assert_eq!(server.workspace.documents["untitled:edits"].text, "中y");
}

#[test]
fn jsonrpc_framing_rejects_truncated_and_duplicate_headers() {
    for invalid in [
        b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        b"Content-Length: 9\r\n\r\n{}",
        b"Content-Length: 1000000000\r\n\r\n",
        b"Content-Length: 2\r\n",
    ] {
        assert!(super::read_message(&mut std::io::Cursor::new(invalid)).is_err());
    }
    let value = serde_json::json!({"id": "中文😀", "result": null});
    let mut output = Vec::new();
    super::write_message(&mut output, value.clone()).unwrap();
    assert_eq!(
        super::read_message(&mut std::io::Cursor::new(output)).unwrap(),
        Some(value)
    );
}

#[test]
fn new_deleted_and_renamed_disk_files_refresh_workspace_symbols() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = crate::workspace::Workspace::default();
    workspace
        .roots
        .insert(crate::workspace::file_uri(directory.path()).unwrap());
    workspace.refresh();
    assert!(workspace.documents.is_empty());
    let path = directory.path().join("first.rest");
    std::fs::write(&path, "# @name first\nGET https://example.invalid").unwrap();
    workspace.refresh();
    let first_uri = crate::workspace::file_uri(&path).unwrap();
    assert!(workspace.documents.contains_key(&first_uri));
    let second_path = directory.path().join("second.http");
    std::fs::rename(path, &second_path).unwrap();
    workspace.refresh();
    assert!(!workspace.documents.contains_key(&first_uri));
    assert_eq!(
        crate::features::symbols(&workspace, "first")[0]["location"]["uri"],
        crate::workspace::file_uri(&second_path).unwrap()
    );
    std::fs::remove_file(second_path).unwrap();
    workspace.refresh();
    assert!(workspace.documents.is_empty());
}

#[test]
fn no_script_plugin_or_request_is_executed() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("must-not-exist");
    let script = format!(
        "{{{{\nrequire('node:fs').writeFileSync({}, 'executed');\n}}}}\nGET http://127.0.0.1:1/must-not-run",
        serde_json::to_string(&marker).unwrap()
    );
    let mut workspace = crate::workspace::Workspace::default();
    workspace.update("untitled:security", script, 1);
    workspace.refresh();
    let diagnostics = crate::features::diagnostics(
        &workspace,
        &workspace.documents["untitled:security"],
        &crate::features::Catalog::load().unwrap(),
    );
    assert!(diagnostics.is_empty());
    assert!(!marker.exists());
}
