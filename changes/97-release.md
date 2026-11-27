Internal

Both crates take one version from the workspace (`version.workspace = true`), and the program depends on the library through `[workspace.dependencies]`. Each crate now carries its license files, so `cargo package` ships them: `LICENSE-MIT` and `LICENSE-APACHE` in `carrysheet`, `LICENSE-GPL` in `carrysheet-cli`. `about.toml` configures `cargo about`, which writes the third-party notices at release time. `CONTRIBUTING.md` says how a release is made from a tag; only the owner tags (issues 94, 95 and 97).
