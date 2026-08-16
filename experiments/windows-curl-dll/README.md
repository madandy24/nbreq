# Windows curl DLL lifecycle probe

This private WP2 experiment builds a Rust `cdylib` that creates a curl-backed NBReq Engine, runs a
local HTTP callback request, drains the callback worker, and shuts the Engine down. A separate host
preloads the pinned `libcurl.dll` by absolute path, verifies the loaded module path, loads the probe,
and calls its export.

The host intentionally pins both DLLs until process exit. The Rust `curl` binding deliberately does
not call `curl_global_cleanup()`, so this experiment does **not** claim that `FreeLibrary`-based
unloading is safe. The curl-backed GDS pilot must remain loaded until its host process exits. The
test repeats fresh host processes to cover ordinary process load/use/exit lifecycle.
