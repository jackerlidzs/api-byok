mod desktop;
#[cfg(not(dev))]
mod frontend;
mod resource_limits;
mod startup;
mod tray;
mod update;

// mimalloc returns freed memory to the operating system; with glibc's retained pages the RSS
// would not drop back to the silent-start level after the window closes.
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub fn run() -> std::process::ExitCode {
    if let Some(exit_code) = update::run_replacement_if_requested() {
        return exit_code;
    }
    desktop::run()
}
