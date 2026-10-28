# RAR fixtures

Oxide cannot create RAR archives, so these small samples are copied from the
test data of the [`unrar`](https://crates.io/crates/unrar) crate (v0.5.8,
MIT OR Apache-2.0).

| File          | Contents                                                 |
|---------------|----------------------------------------------------------|
| `version.rar` | `VERSION`                                                |
| `crypted.rar` | `.gitignore`, data encrypted with password `unrar`       |
| `solid.rar`   | solid archive                                            |
| `unicode.rar` | entry with a non-ASCII file name                         |
