mod desktop;
mod histogram;

fn main() {
    if let Err(error) = desktop::run() {
        eprintln!("Shadowroom: {error:#}");
        std::process::exit(1);
    }
    // The orderly quit path has already confirmed the catalog/settings flush and
    // waited for exports and imports. Skip C/C++ static destructors: dynamically
    // loaded decoders (libheif) may still be running on preview worker threads and
    // would race their own global teardown (observed SIGSEGV in libheif at exit).
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    unsafe { libc::_exit(0) }
}
