Changed

`overview` no longer takes `response_format`; it only widened the header row, which `overview` no longer shows (record 0010, decision 11). The `confirmation_required` error no longer repeats the whole call in its fix: it says to call again with the same arguments and `"confirm":true`, or to add `--confirm` on the command line. The error code `declined` is gone; nothing asks a question any more, so no command returned it. The texts of `convert` name xlsx and csv only.
