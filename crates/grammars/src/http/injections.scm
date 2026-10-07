; Structured request/response bodies.
((json_body) @injection.content
  (#set! injection.language "json"))

((xml_body) @injection.content
  (#set! injection.language "xml"))

((graphql_data) @injection.content
  (#set! injection.language "graphql"))

; httpYac native scripts and IntelliJ handlers use JavaScript by default.
((native_script
  body: (script_body) @injection.content)
  (#set! injection.language "javascript"))

((script
  body: [
    (script_body)
    (inline_script_body)
  ] @injection.content)
  (#set! injection.language "javascript"))
