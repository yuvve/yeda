# yeda-fs

Loads a library from a local folder and applies yeda-core's `Effect`s to it, for every client that uses `std::fs`.

All writes are atomic (a temporary file renamed into place), because several clients may write the same library at once.
