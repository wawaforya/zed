; Redact credentials in common authentication and session headers.
(
  (header
    name: (header_entity) @_sensitive_header
    value: (value) @redact)
  (#match? @_sensitive_header "(?i)^(authorization|proxy-authorization|cookie|set-cookie|x-api-key|x-auth-token|x-access-token|api-key)$")
)

; Redact only the value of variables whose names indicate credentials.
(
  (variable_declaration
    name: (identifier) @_sensitive_variable
    value: (value) @redact)
  (#match? @_sensitive_variable "(?i)(password|passwd|secret|token|api[-_.]?key)")
)

(
  (lazy_variable_declaration
    name: (identifier) @_sensitive_variable
    value: (value) @redact)
  (#match? @_sensitive_variable "(?i)(password|passwd|secret|token|api[-_.]?key)")
)
