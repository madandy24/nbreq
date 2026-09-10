# Windows / Wine DNS probe

Unpublished diagnostic tool for W-01. Build a **debug 32-bit Windows executable**, so alignment
failures remain observable. Do not disable alignment or unsafe precondition checks.

```powershell
cargo build --manifest-path tools/wine-dns-probe/Cargo.toml --target i686-pc-windows-msvc
# Also test the GDS-like native-only feature set:
cargo build --manifest-path tools/wine-dns-probe/Cargo.toml --target i686-pc-windows-msvc --no-default-features
```

With no argument, inspect GetAdaptersAddresses' owned buffer, structure offsets and descriptive
pointer bounds/alignment/termination before any string conversion. `--legacy` calls unchanged
ipconfig 0.3.4/widestring 1.2.1 in a separate process; on affected Wine it is expected to abort.
`--fixed` calls the new bounded support wrapper. An `http://` argument exercises ordinary Engine
construction, the supplied loopback HTTP fixture, public DNS (when enabled), hostname HTTP to
example.com and joined shutdown. No DNS server override is injected.

`run.py --binary /absolute/path/probe.exe --out /new/results` runs the non-failing modes with a
local HTTP fixture and bounded subprocesses. Add `--wine wine --prefix /absolute/private/prefix`
for Wine. The runner requires ordinary external DNS and HTTP connectivity to example.com.
It does not modify any GDS files, databases, OS DNS configuration or existing application prefix.

Wine 5 needs the existing `experiments/wine5-bcryptprimitives` ProcessPrng shim next to these
modern Rust executables. Record its source and binary hash; do not install it into prefix/system
directories. This is a separate loader prerequisite and does not change DNS parsing or alignment.
After testing, stop only the private prefix's wineserver if needed; never terminate other prefixes.

The old dependency remains solely in this diagnostic tool to preserve reproduction. It is absent
from NBReq's production dependency graph and published package. Record Wine version, architecture,
prefix, compiler, exact binary hashes and actual outputs before drawing compatibility conclusions.
