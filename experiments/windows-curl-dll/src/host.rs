#[cfg(not(windows))]
compile_error!("the NBReq curl DLL host probe is Windows-only");

use std::env;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

type Probe = unsafe extern "C" fn() -> i32;

fn main() {
    let mut args = env::args_os().skip(1);
    let curl_path = absolute(args.next().expect("usage: host <libcurl.dll> <probe.dll>"));
    let probe_path = absolute(args.next().expect("usage: host <libcurl.dll> <probe.dll>"));
    assert!(args.next().is_none(), "too many arguments");

    unsafe {
        let curl_module = load_exact(&curl_path);
        let actual_curl = module_path(curl_module);
        assert!(
            paths_equal(&actual_curl, &curl_path),
            "loaded curl from {}, expected {}",
            actual_curl.display(),
            curl_path.display()
        );

        let probe_module = load_exact(&probe_path);
        let symbol = GetProcAddress(probe_module, c"nbreq_curl_dll_probe".as_ptr().cast());
        let symbol = symbol.expect("probe export was not found");
        let probe: Probe =
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Probe>(symbol);
        let result = probe();
        assert_eq!(result, 0, "DLL probe returned failure code {result}");

        // Raw HMODULE values have no Drop implementation. By deliberately omitting FreeLibrary,
        // the curl pilot pins both modules until process exit. The Rust binding does not call
        // curl_global_cleanup(), so FreeLibrary-based unload is not claimed safe.
    }
}

fn absolute(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .canonicalize()
        .expect("probe path must exist and resolve")
}

unsafe fn load_exact(path: &Path) -> HMODULE {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut::<c_void>(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    assert!(!module.is_null(), "failed to load {}", path.display());
    module
}

unsafe fn module_path(module: HMODULE) -> PathBuf {
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    assert_ne!(length, 0, "GetModuleFileNameW failed");
    buffer.truncate(length as usize);
    PathBuf::from(String::from_utf16(&buffer).expect("loaded module path must be UTF-16"))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}
