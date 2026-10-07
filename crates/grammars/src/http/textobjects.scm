; Treat each request section as a function-like unit for Vim navigation and
; selection (`[m`, `]m`, `af`).
(section) @function.around

; Request bodies are the inside of the request (`if`).
(request
  body: [
    (raw_body)
    (multipart_form_data)
    (xml_body)
    (json_body)
    (graphql_body)
    (external_body)
  ] @function.inside)

[
  (native_script)
  (script)
] @function.around

[
  (script_body)
  (inline_script_body)
] @function.inside

; Adjacent comments form one comment text object.
(comment)+ @comment.around
