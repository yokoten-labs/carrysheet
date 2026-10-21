# Third-party material

## numbers-parser

<https://github.com/masaccio/numbers-parser>, at commit
`a6affe5599c8373f575192ac5777d0c6426da696`.

What this crate takes from it:

- `proto/*.proto`: trimmed copies of the protobuf definitions in
  `src/protos/` (only the messages and fields this crate reads; field
  numbers and types unchanged). `src/proto/*.rs` is generated from them.
- `src/model/functions.rs`: the table of formula function names by index,
  from `src/numbers_parser/generated/functionmap.py`.
- `tests/data/`: test documents from `tests/data/`, with previews and images
  removed; see `tests/data/README.md`.
- Format knowledge from its source code: the IWA chunk framing, the cell
  storage layout, the decimal number encoding, the order formula nodes are
  read in, and where merged ranges are kept.

Its documentation file `docs/api/file-format.rst` is under CC BY-NC-SA 4.0
and was not used.

```text
Copyright 2021 Jon Connell

Permission is hereby granted, free of charge, to any person obtaining a copy of this software
and associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE
```

numbers-parser credits these MIT-licensed projects for the protobuf
definitions and the IWA format, which its definitions derive from:

### keynote-parser

<https://github.com/psobot/keynote-parser>

```text
Copyright 2019-2025 Peter Sobot

Permission is hereby granted, free of charge, to any person obtaining a copy of this software
and associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

### iWorkFileFormat

<https://github.com/obriensp/iWorkFileFormat>

```text
The MIT License (MIT)

Copyright (c) 2013 Sean Patrick O'Brien (http://obriensp.com)

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## Snappy

`src/iwa.rs` decodes raw Snappy blocks with its own code, written from
Google's Snappy format description (`format_description.txt`, BSD 3-Clause).
No Snappy code was copied.

## Calendar arithmetic

`civil_from_days` in `src/model/cell.rs` follows Howard Hinnant's public
domain date algorithms.
