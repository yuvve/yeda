# yeda-fs

The client side of yeda-core's I/O, for clients that use `std::fs`. yeda-tui, yeda-reader and yeda-android all use it.

- `load_library(dir, machine_config, home)` reads `yeda.toml`, the device config, every `refs/*.toml` and the LLM's table rows, and returns the parsed `Library`.
- `apply(dir, effects)` performs yeda-core's `Effect`s inside the library folder.
- `write_atomic(path, bytes)` writes a temporary file and renames it into place, so concurrent readers never see a partial file.
