fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    // sk-iced builders produce rich native widget trees; Windows' default
    // one-megabyte main stack is too small for debug builds of these views.
    if target.contains("windows-msvc") {
        println!("cargo:rustc-link-arg=/STACK:33554432");
    } else if target.contains("windows") {
        println!("cargo:rustc-link-arg=-Wl,--stack,33554432");
    }
    if target.contains("windows") {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let resource = out.join(if target.contains("msvc") {
            "icon.res"
        } else {
            "icon.o"
        });
        let mut command = if target.contains("msvc") {
            let mut command = std::process::Command::new("rc.exe");
            command
                .arg("/nologo")
                .arg(format!("/fo{}", resource.display()));
            command
        } else {
            let mut command = std::process::Command::new("windres");
            command.args(["-O", "coff", "-o"]).arg(&resource);
            command
        };
        let status = command
            .arg("assets/windows.rc")
            .status()
            .expect("Windows resource compiler (rc.exe or windres) is required");
        assert!(status.success(), "Compile Windows app icon");
        println!("cargo:rustc-link-arg={}", resource.display());
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/saucecache.ico");
    }
}
