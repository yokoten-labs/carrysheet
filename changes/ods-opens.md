Fixed

`.ods` files open, as the README and record 0010, decision 3 say. The extension check let only `.xlsx`, `.xlsm`, `.xltx`, `.xltm`, `.xls`, `.xlsb` and `.csv` through, although GridCraft already read the file by its content. `overview` says that macros in an .ods file were not checked; Carrysheet never runs them. Saving is still to `.xlsx` or `.csv` (record 0011).
