; Indent structured and raw request bodies relative to their request line.
[
  (raw_body)
  (json_body)
  (xml_body)
  (graphql_body)
  (multipart_form_data)
] @indent

; Script contents are indented until the closing delimiter.
(script
  "%}" @end) @indent

(native_script
  "}}" @end) @indent
