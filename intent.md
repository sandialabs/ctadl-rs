# Import: read only the bytes a step needs

`ctadl import` holds whole artifacts in memory when it needs only part of them, or needs
them only as a stream. Hashing an artifact reads it whole, on a thread that runs alongside
the import. The APK import reads the whole package before parsing it. So a 180 MB APK is in
memory twice. Deciding whether a `.dll`/`.exe` is .NET reads the whole file too.

- Hash artifacts by streaming them, with every digest unchanged.
- Import an APK without copying it into memory first: map it, and give the mapping to the
  existing APK reader.
- Decide whether a `.dll`/`.exe` is .NET without reading the whole file.

Changing how the APK reader reads an archive is out of scope.

Import output must not change.
