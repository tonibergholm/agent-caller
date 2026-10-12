fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // The voice bridge (plan S1). swift-rs runs `swift build` on the
        // package, adds the Swift runtime search paths and links the static
        // library. Swift autolinks AVFoundation, Speech, CoreGraphics and
        // IOKit through its imports.
        swift_rs::SwiftLinker::new("13.0").with_package("VoiceBridge", "swift").link();
        // The package turns autolinking off for FoundationModels (plan S16);
        // link it weakly, if this SDK has it, so the app launches before macOS 26.
        let sdk = std::process::Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]).output();
        if let Ok(out) = sdk {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if std::path::Path::new(&path).join("System/Library/Frameworks/FoundationModels.framework").exists() {
                println!("cargo:rustc-link-arg=-Wl,-weak_framework,FoundationModels");
            }
        }
        println!("cargo:rerun-if-changed=swift/Sources");
        println!("cargo:rerun-if-changed=swift/Package.swift");
    }
    tauri_build::build()
}
