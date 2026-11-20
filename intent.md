# Import: read only the bytes a step needs

`ctadl import` holds whole artifacts in memory when it needs only part of them, or needs
them only as a stream. On a 180 MB APK with 7.6 MB of Dex, that costs ~225 MB of peak memory.

- Hash artifacts by streaming them, with every digest unchanged.
- Import an APK from the file, decompressing only its `classes*.dex` entries.
- Decide whether a `.dll`/`.exe` is .NET from its PE headers alone.

Import output must not change.
