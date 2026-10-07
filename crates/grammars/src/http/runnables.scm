; Named requests expose ZED_CUSTOM_name, method, and url.
(
  (section
    (metadata
      (name_metadata
        value: (value) @name))
    request: (request
      method: (method) @run @method
      url: (target_url) @url) @http_request)
  (#set! tag http-request-named)
)

; Named bare-URL requests remain runnable.
(
  (section
    (metadata
      (name_metadata
        value: (value) @name))
    request: (request
      !method
      url: (target_url) @run @url) @http_request)
  (#match? @url "(?i)^(https?://|wss?://|grpc://|mqtts?://|amqps?://|\\{\\{)")
  (#set! tag http-request-named)
)

; Standard unnamed HTTP/httpYac requests.
(
  (section
    request: (request
      method: (method) @run @method
      url: (target_url) @url) @http_request) @_section
  (#not-match? @_section "(?m)^\\s*(#|//)\\s*@name(\\s|=)")
  (#set! tag http-request)
)

; Bare URL requests are legal in httpYac.
(
  (section
    request: (request
      !method
      url: (target_url) @run @url) @http_request) @_section
  (#not-match? @_section "(?m)^\\s*(#|//)\\s*@name(\\s|=)")
  (#match? @url "(?i)^(https?://|wss?://|grpc://|mqtts?://|amqps?://|\\{\\{)")
  (#set! tag http-request)
)
