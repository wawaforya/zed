/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
    VAR_COMMENT_PREFIX: 2,
    BODY_PREFIX: 2,
    RAW_BODY: 3,
    GRAPQL_JSON_PREFIX: 4,
    COMMENT_PREFIX: 5,
    BODY_SEPARATOR: 7,
    SCRIPT_DELIMITER: 8,
    REQ_SEPARATOR: 9,
};

const WORD_CHAR = /[\p{L}\p{N}]/u;
const PUNCTUATION = /[^\n\r\p{Z}\p{L}\p{N}]/u;
const WS = /\p{Zs}+/u;
const NL = token(choice("\n", "\r", "\r\n", "\0"));
const LINE_TAIL = token(seq(/.*/, NL));
const ESCAPED = token(/\\[^\n\r]/);
const COMMENT_PREFIX = token(
    prec(PREC.COMMENT_PREFIX, choice(/#\s*/, /\/\/\s*/)),
);

const metadataRule = (keyword) => ($) =>
    seq(
        COMMENT_PREFIX,
        token(prec(PREC.VAR_COMMENT_PREFIX, "@")),
        field("name", alias(keyword, $.metadata_key)),
        optional(
            seq(
                choice(WS, "="),
                optional(token(prec(1, WS))),
                optional(field("value", $.value)),
            ),
        ),
        NL,
    );

module.exports = grammar({
    name: "httpyac",

    extras: (_) => [],
    conflicts: ($) => [
        [$.target_url],
        [$.raw_body],
        [$._raw_body],
        [$._section_content],
    ],
    inline: ($) => [$._target_url_line, $.__body],

    rules: {
        document: ($) => repeat($.section),
        // NOTE: just for debugging purpose
        WORD_CHAR: (_) => WORD_CHAR,
        PUNCTUATION: (_) => PUNCTUATION,
        WS: (_) => WS,
        NL: (_) => NL,
        LINE_TAIL: (_) => LINE_TAIL,
        COMMENT_PREFIX: (_) => COMMENT_PREFIX,

        comment: ($) => $._plain_comment,
        _plain_comment: (_) =>
            seq(
                COMMENT_PREFIX,
                LINE_TAIL,
            ),
        metadata: ($) =>
            choice(
                $.name_metadata,
                $.ref_metadata,
                $.force_ref_metadata,
                $.import_metadata,
                $.generic_metadata,
            ),
        name_metadata: metadataRule("name"),
        ref_metadata: metadataRule("ref"),
        force_ref_metadata: metadataRule("forceRef"),
        import_metadata: metadataRule("import"),
        generic_metadata: ($) =>
            seq(
                COMMENT_PREFIX,
                token(prec(PREC.VAR_COMMENT_PREFIX, "@")),
                field("name", $.identifier),
                optional(
                    seq(
                        choice(WS, "="),
                        optional(token(prec(1, WS))),
                        field("value", $.value),
                    ),
                ),
                NL,
            ),
        metadata_key: (_) => /[A-Za-z][A-Za-z\d]*/,

        request_separator: ($) =>
            seq(
                token(prec(PREC.REQ_SEPARATOR, /###+\p{Zs}*/)),
                optional(token(prec(1, WS))),
                optional(field("value", $.value)),
                NL,
            ),

        section: ($) =>
            prec.right(
                choice(
                    seq($.request_separator, optional($._section_content)),
                    $._section_content,
                ),
            ),

        // NOTE: grammatically, each request section should contain only single `$.request` node
        // we are allowing multiple `$.request` nodes here to lower the parser size
        _section_content: ($) =>
            choice(
                seq($._blank_line, optional($._section_content)),
                seq($.comment, optional($._section_content)),
                seq($.metadata, optional($._section_content)),
                seq($.variable_declaration, optional($._section_content)),
                seq($.lazy_variable_declaration, optional($._section_content)),
                seq($.native_script, optional($._section_content)),
                seq($.pre_request_script, optional($._section_content)),
                // field to easily find request node in each section
                field("request", $.request),
                field("response", $.response),
            ),

        // HTTP/WebDAV methods and the protocol-leading verbs supported by
        // httpYac 6.x. LIST is retained for compatibility with the upstream
        // grammar and existing vault-oriented files.
        method: (_) =>
            /(BASELINE-CONTROL|VERSION-CONTROL|PROPFIND|PROPPATCH|MKACTIVITY|MKWORKSPACE|MKCALENDAR|CHECKOUT|CHECKIN|OPTIONS|CONNECT|GRAPHQL|WEBSOCKET|EVENTSOURCE|MQTTS|REPORT|SEARCH|UNLOCK|DELETE|GRPC|TRACE|PATCH|MERGE|MKCOL|COPY|MOVE|LOCK|ACL|LIST|HEAD|POST|PUT|GET|WSS|WS|SSE|MQTT|AMQP)/,

        http_version: (_) => prec.dynamic(1, token(prec(0, /HTTP\/[\d\.]+/))),

        _target_url_line: ($) =>
            repeat1(choice(WORD_CHAR, PUNCTUATION, $.variable, WS)),
        target_url: ($) =>
            seq($._target_url_line, repeat(seq(NL, WS, $._target_url_line))),

        status_code: (_) => /[1-5]\d{2}/,
        status_text: (_) =>
            /(Continue|Switching Protocols|Processing|OK|Created|Accepted|Non-Authoritative Information|No Content|Reset Content|Partial Content|Multi-Status|Already Reported|IM Used|Multiple Choices|Moved Permanently|Found|See Other|Not Modified|Use Proxy|Switch Proxy|Temporary Redirect|Permanent Redirect|Bad Request|Unauthorized|Payment Required|Forbidden|Not Found|Method Not Allowed|Not Acceptable|Proxy Authentication Required|Request Timeout|Conflict|Gone|Length Required|Precondition Failed|Payload Too Large|URI Too Long|Unsupported Media Type|Range Not Satisfiable|Expectation Failed|I'm a teapot|Misdirected Request|Unprocessable Entity|Locked|Failed Dependency|Too Early|Upgrade Required|Precondition Required|Too Many Requests|Request Header Fields Too Large|Unavailable For Legal Reasons|Internal Server Error|Not Implemented|Bad Gateway|Service Unavailable|Gateway Timeout|HTTP Version Not Supported|Variant Also Negotiates|Insufficient Storage|Loop Detected|Not Extended|Network Authentication Required)/,
        __body: ($) =>
            seq(
                repeat1($._blank_line),
                prec.right(
                    repeat(
                        choice(
                            $.metadata,
                            field(
                                "body",
                                choice(
                                    $.raw_body,
                                    $.multipart_form_data,
                                    $.xml_body,
                                    $.json_body,
                                    $.graphql_body,
                                    $._external_body,
                                ),
                            ),
                            NL,
                            $.assertion,
                            $.native_script,
                            $.message_separator,
                            $.res_handler_script,
                            $.res_redirect,
                        ),
                    ),
                ),
            ),
        response: ($) =>
            prec.right(
                seq(
                    $.http_version,
                    WS,
                    $.status_code,
                    WS,
                    optional($.status_text),
                    NL,
                    repeat(field("header", $.header)),
                    optional($.__body),
                ),
            ),

        request: ($) =>
            prec.right(
                seq(
                    optional(seq(field("method", $.method), WS)),
                    field("url", $.target_url),
                    optional(seq(WS, field("version", $.http_version))),
                    NL,
                    repeat(
                        choice(
                            $.comment,
                            $.metadata,
                            $.proto_import,
                            field("header", $.header),
                        ),
                    ),
                    optional($.__body),
                ),
            ),

        query_param: ($) =>
            prec.right(
                seq(
                    field("key", $.value),
                    optional(seq("=", optional(field("value", $.value)))),
                ),
            ),

        header: ($) =>
            seq(
                field("name", $.header_entity),
                optional(WS),
                ":",
                optional(token(prec(1, WS))),
                optional(field("value", choice($.value))),
                NL,
            ),

        // {{foo}} {{$bar}} {{ fizzbuzz }}
        variable: ($) =>
            seq(
                token(prec(1, "{{")),
                optional(WS),
                field(
                    "name",
                    choice(
                        $.variable_name,
                        $.variable_expression,
                    ),
                ),
                optional(WS),
                token(prec(1, "}}")),
            ),
        variable_name: (_) =>
            token(
                prec(
                    2,
                    /[A-Za-z_\$\d\u00A1-\uFFFF-]+(?:\.[A-Za-z_\$\d\u00A1-\uFFFF-]+)*(?:\([^{}\n\r]*\))?/,
                ),
            ),
        variable_expression: (_) => token(prec(-1, /[^{}\n\r]+/)),

        pre_request_script: ($) =>
            seq("<", WS, choice($.script, $.path), token(repeat1(NL))),
        res_handler_script: ($) =>
            seq(
                token(prec(PREC.REQ_SEPARATOR, ">")),
                WS,
                choice($.script, $.path),
                token(repeat1(NL)),
            ),
        script: ($) =>
            choice(
                seq(
                    token(prec(PREC.SCRIPT_DELIMITER, "{%")),
                    NL,
                    optional(field("body", $.script_body)),
                    token(prec(PREC.SCRIPT_DELIMITER, "%}")),
                ),
                seq(
                    token(prec(PREC.SCRIPT_DELIMITER, "{%")),
                    optional(WS),
                    optional(field("body", $.inline_script_body)),
                    optional(WS),
                    token(prec(PREC.SCRIPT_DELIMITER, "%}")),
                ),
            ),
        script_body: (_) => repeat1(LINE_TAIL),
        inline_script_body: (_) =>
            token(repeat1(choice(/[^%\n\r]/, /%[^}]/))),

        native_script: ($) =>
            seq(
                token(prec(PREC.SCRIPT_DELIMITER, "{{")),
                optional(
                    seq(
                        field("language", alias("@js", $.script_language)),
                        WS,
                    ),
                ),
                optional(field("modifier", $.script_modifier)),
                optional(field("event", $.script_event)),
                optional(WS),
                NL,
                optional(field("body", $.script_body)),
                token(prec(PREC.SCRIPT_DELIMITER, "}}")),
                optional(WS),
                NL,
            ),
        script_language: (_) => "@js",
        script_modifier: (_) => choice("+", "@"),
        script_event: (_) =>
            choice(
                "request",
                "streaming",
                "response",
                "after",
                "responseLogging",
            ),

        assertion: ($) =>
            seq(
                token(prec(PREC.BODY_SEPARATOR, "??")),
                optional(WS),
                field("type", $.assertion_type),
                optional(
                    seq(
                        WS,
                        field("expression", $.assertion_expression),
                    ),
                ),
                NL,
            ),
        assertion_type: (_) => /[^\s\n\r]+/,
        assertion_expression: (_) => /[^\n\r]+/,

        message_separator: ($) =>
            seq(
                token(prec(PREC.BODY_SEPARATOR, "===")),
                optional(
                    seq(
                        WS,
                        optional(field("value", $.value)),
                    ),
                ),
                NL,
            ),

        proto_import: ($) =>
            seq(
                "proto",
                WS,
                "<",
                WS,
                field("path", $.path),
                NL,
            ),

        res_redirect: ($) =>
            seq(
                token(prec(PREC.REQ_SEPARATOR, />>!?/)),
                WS,
                field("path", $.path),
                token(repeat1(NL)),
            ),

        variable_declaration: ($) =>
            seq(
                "@",
                optional(field("scope", $.variable_scope)),
                field("name", $.identifier),
                optional(WS),
                "=",
                optional(token(prec(1, WS))),
                field("value", $.value),
                NL,
            ),
        lazy_variable_declaration: ($) =>
            seq(
                "@",
                optional(field("scope", $.variable_scope)),
                field("name", $.identifier),
                optional(WS),
                ":=",
                optional(token(prec(1, WS))),
                field("value", $.value),
                NL,
            ),
        variable_scope: (_) => token(prec(4, "global.")),

        xml_body: ($) =>
            seq(token(prec(PREC.BODY_PREFIX, /<[^\s@]/)), $._raw_body),

        json_body: ($) =>
            seq(token(prec(PREC.BODY_PREFIX, /[{\[]\s+/)), $._raw_body),

        graphql_body: ($) =>
            prec.right(
                seq(
                    $.graphql_data,
                    optional(alias($.graphql_json_body, $.json_body)),
                ),
            ),
        graphql_data: ($) =>
            seq(
                token(
                    prec(
                        PREC.BODY_PREFIX,
                        seq(choice("query", "mutation"), WS, /.*\{/, NL),
                    ),
                ),
                $._raw_body,
            ),
        graphql_json_body: ($) =>
            seq(token(prec(PREC.GRAPQL_JSON_PREFIX, /[{\[]\s+/)), $._raw_body),

        _external_body: ($) => seq($.external_body, NL),
        external_body: ($) =>
            seq(
                token(prec(PREC.BODY_PREFIX, "<")),
                optional(seq("@", optional(field("name", $.identifier)))),
                WS,
                field("path", $.path),
            ),

        multipart_form_data: ($) =>
            prec.right(
                seq(
                    token(prec(PREC.BODY_PREFIX, "--")),
                    token(prec(1, LINE_TAIL)),
                    repeat(
                        choice(
                            $.comment,
                            seq($.external_body, choice(WS, NL)),
                            token(prec(2, /<[^\s@]/)),
                            token(prec(2, "--")),
                            token(prec(2, /[{\[]\s+/)),
                            token(prec(1, LINE_TAIL)),
                            token(prec(2, NL)),
                        ),
                    ),
                ),
            ),

        raw_body: ($) =>
            seq(
                choice(
                    token(prec(1, seq(/.+/, NL))),
                    seq(COMMENT_PREFIX, $._not_comment),
                ),
                optional($._raw_body),
            ),
        _raw_body: ($) =>
            seq(
                choice(
                    token(prec(PREC.RAW_BODY, LINE_TAIL)),
                    seq(COMMENT_PREFIX, $._not_comment),
                ),
                optional($._raw_body),
            ),
        // Keep the non-metadata tail on the current line. A negated character
        // class that excludes only `@` also matches newlines, so a `//` body
        // comment could otherwise consume response handlers and separators up
        // to the next metadata line.
        _not_comment: (_) => token(seq(/[^@\r\n]*/, NL)),

        header_entity: (_) => /[\w\-]+/,
        identifier: (_) => /[A-Za-z_.\$\d\u00A1-\uFFFF-]+/,
        path: ($) =>
            prec.right(
                repeat1(choice(WORD_CHAR, PUNCTUATION, $.variable, ESCAPED)),
            ),
        value: ($) => repeat1(choice(WORD_CHAR, PUNCTUATION, $.variable, WS)),
        _blank_line: (_) => seq(optional(WS), token(prec(-1, NL))),
    },
});
