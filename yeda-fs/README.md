# yeda-fs

Part of [yeda](https://github.com/yuvve/yeda), and not meant to be used on its own: install yeda with `cargo install yeda`.

Loads a library from a local folder and applies yeda-core's `Effect`s to it, for every client that uses `std::fs`.

All writes are atomic (a temporary file renamed into place), because several clients may write the same library at once.
