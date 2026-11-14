// Qualification-only helper: no console output or debugger attach.
#[cfg(all(windows, target_arch = "x86_64"))]
#[path = "../src/automation_probe/fatal_dump.rs"]
mod fatal_dump;

fn main() {
    #[cfg(all(windows, target_arch = "x86_64"))]
    fatal_dump::helper_main();
}
