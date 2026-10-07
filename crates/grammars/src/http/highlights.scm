; Request and response lines.
(method) @keyword
(request
  url: (target_url) @keyword)
(http_version) @constant
(status_code) @number
(status_text) @string

; Headers.
(header
  name: (header_entity) @property
  ":" @punctuation.delimiter
  value: (value)? @string)

; Eager and lazy file variables.
[
  (variable_declaration
    "@" @punctuation.special
    scope: (variable_scope)? @keyword
    name: (identifier) @variable
    "=" @operator
    value: (value) @string)
  (lazy_variable_declaration
    "@" @punctuation.special
    scope: (variable_scope)? @keyword
    name: (identifier) @variable
    ":=" @operator
    value: (value) @string)
]

; Variable references and expressions.
(variable
  "{{" @punctuation.bracket
  name: [
    (variable_name)
    (variable_expression)
  ] @variable
  "}}" @punctuation.bracket)

; Explicit httpYac metadata.
[
  (name_metadata
    "@" @punctuation.special
    name: (metadata_key) @keyword
    value: (value)? @label)
  (ref_metadata
    "@" @punctuation.special
    name: (metadata_key) @keyword
    value: (value)? @variable)
  (force_ref_metadata
    "@" @punctuation.special
    name: (metadata_key) @keyword
    value: (value)? @variable)
  (import_metadata
    "@" @punctuation.special
    name: (metadata_key) @keyword
    value: (value)? @string.special)
  (generic_metadata
    "@" @punctuation.special
    name: (identifier) @keyword
    value: (value)? @string)
]

; Request section titles.
(request_separator
  value: (value)? @comment)

; Assertions.
(assertion
  "??" @keyword
  type: (assertion_type) @type
  expression: (assertion_expression)? @string)

; JavaScript and IntelliJ handler syntax.
(native_script
  language: (script_language)? @attribute
  modifier: (script_modifier)? @operator
  event: (script_event)? @keyword
  body: (script_body)? @embedded)

[
  (script)
  (script_body)
  (inline_script_body)
  (raw_body)
  (json_body)
  (xml_body)
  (graphql_data)
] @embedded

; Paths and response redirects.
(external_body
  path: (path) @string.special)
(proto_import
  path: (path) @string.special)
(pre_request_script
  (path) @string.special)
(res_handler_script
  (path) @string.special)
(res_redirect
  path: (path) @string.special)

(message_separator
  "===" @punctuation.delimiter
  value: (value)? @keyword)

[
  "{{"
  "}}"
  "{%"
  "%}"
] @punctuation.bracket

; General comments are last so metadata children keep their specific styles.
[
  (comment)
  (request_separator)
] @comment
