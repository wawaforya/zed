; Named requests: show the httpYac name, method, and URL.
(
  (section
    (metadata
      (name_metadata
        value: (value) @name))
    request: (request
      method: (method)? @context
      url: (target_url) @name) @item)
)

; Unnamed requests: show method and URL, excluding explicit name metadata.
(
  (section
    request: (request
      method: (method)? @context
      url: (target_url) @name) @item) @_section
  (#not-match? @_section "(?m)^\\s*(#|//)\\s*@name(\\s|=)")
)
