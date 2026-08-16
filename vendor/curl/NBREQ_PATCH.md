# NBReq's curl 0.4.50 patch

NBReq's Windows DLL pilot must not call `curl_global_init()` from a CRT constructor while the
Windows loader lock may be held. Upstream `curl` 0.4.50 installs such a constructor on Windows (and
equivalent startup hooks on several executable platforms).

The local `nbreq-explicit-init` feature disables only that constructor. NBReq calls `curl::init()`
explicitly while constructing the curl backend on its spawned reactor thread. The upstream crate's
`Once` guard and deliberate no-`curl_global_cleanup()` policy are otherwise unchanged.

Upstream source revision recorded by the published crate: `0cfd9e3b8b1aa0b8fc2c8d552597555a30a21416`.
Review or upstream this patch before changing the pinned `curl` crate version.
