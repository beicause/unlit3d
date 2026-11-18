//! The logger a run installs before it reads its command line.

/// Install the platform's logger, once.
///
/// A browser logs to the console and turns a panic into a console error; a
/// desktop run reads `RUST_LOG` and defaults to `info`; an Android activity
/// logs to logcat. Calling it twice is harmless: the logger is already set.
pub fn init() {
    #[cfg(target_arch = "wasm32")]
    {
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        let _ = console_log::init_with_level(log::Level::Info);
    }
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
    }
}
