fn main() {
    println!("cargo:rerun-if-changed=assets/animref.ico");
    #[cfg(windows)]
    {
        if let Some(sdk_bin) = windows_sdk_bin() {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![sdk_bin];
            paths.extend(std::env::split_paths(&path));
            if let Ok(path) = std::env::join_paths(paths) {
                // winres invokes rc.exe by name; expose the installed SDK resource compiler.
                unsafe { std::env::set_var("PATH", path) };
            }
        }
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/animref.ico");
        resource.set("FileDescription", "AnimRef");
        resource.set("ProductName", "AnimRef");
        resource.compile().expect("embed AnimRef Windows icon");
    }
}

#[cfg(windows)]
fn windows_sdk_bin() -> Option<std::path::PathBuf> {
    let program_files = std::env::var_os("ProgramFiles(x86)")?;
    let bins = std::path::PathBuf::from(program_files)
        .join("Windows Kits")
        .join("10")
        .join("bin");
    let mut candidates: Vec<_> = std::fs::read_dir(bins)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64"))
        .filter(|path| path.join("rc.exe").is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}
