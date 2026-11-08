// A non-injecting helper that checks the pinned injector's unusual argv contract.
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 || args[0].parse::<u32>().ok().is_none_or(|pid| pid == 0) || args[1] != "0" {
        std::process::exit(29);
    }
    let executable = std::env::current_exe().unwrap();
    let root = executable.parent().unwrap().parent().unwrap();
    let startup_name: Vec<u16> = format!(
        "Local\\ProtoInputMultiPlex.InjectorFixture.{}\0",
        root.file_name().unwrap().to_string_lossy()
    )
    .encode_utf16()
    .collect();
    // Signal only after this non-injecting executable has actually begun running.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(
            attributes: *const core::ffi::c_void,
            manual: i32,
            initial: i32,
            name: *const u16,
        ) -> *mut core::ffi::c_void;
        fn SetEvent(event: *mut core::ffi::c_void) -> i32;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        let event = CreateEventW(core::ptr::null(), 1, 0, startup_name.as_ptr());
        if event.is_null() || SetEvent(event) == 0 {
            std::process::exit(30);
        }
        CloseHandle(event);
    }
    let mode = executable
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    if mode.ends_with("-wait") {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    if mode.ends_with("-fail") {
        std::process::exit(17);
    }
}
