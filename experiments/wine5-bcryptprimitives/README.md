# Wine 5 `bcryptprimitives` compatibility probe

Rust's Windows standard library can import the documented Windows
`bcryptprimitives.dll!ProcessPrng` entry point. Wine 5 predates that DLL, so a current Windows Rust
executable can fail in the loader before `main`, even when all application and libcurl imports are
otherwise satisfied.

This experiment builds a one-export compatibility DLL. Its `ProcessPrng` delegates to
`BCryptGenRandom`, which Wine 5 already supplies. It contains no random-number generator and is not
linked into NBReq or libcurl. The MSVC build has no DLL entry point and does not link a C runtime;
its only functional import is `bcrypt.dll!BCryptGenRandom`.

The source is an NBReq compatibility implementation written against the documented Windows API.
It does not copy or redistribute a Microsoft DLL, a Wine DLL, or Wine implementation source.

Build it with a Visual Studio x64 toolchain:

```powershell
cmake -S . -B build -A x64
cmake --build build --config Release
```

For the Wine 5 proof only, place the resulting `bcryptprimitives.dll` beside the Windows test
executable. Do not install it into Wine's prefix or system directories. A real GDS artifact should
be checked separately: this shim is needed only if that artifact itself imports
`bcryptprimitives.dll`.
