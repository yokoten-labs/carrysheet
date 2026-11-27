Security

Text from a file is escaped by one helper, `carrysheet::text`, in messages, JSON results, the text form and the MCP log (issue 67). Tag characters (U+E0000 to U+E007F), every other invisible or format character from a Unicode 18.0.0 table, and private use characters now show as `\u{…}`, and a backslash shows as `\\`, so file text cannot hide instructions or fake an escape. Plain text, CJK text and emoji do not change. A cut text now ends in `…[cut: N more characters]` instead of `...`, and in a quoted part name the mark follows the closing quote.
