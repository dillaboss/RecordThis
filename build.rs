fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI failed to compile");

    // Icon, version info and the DPI/common-controls manifest for the Windows executable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/recordthis.rc");
        println!("cargo:rerun-if-changed=assets/app.manifest");
        println!("cargo:rerun-if-changed=assets/app.ico");
        println!("cargo:rerun-if-changed=assets/recording.ico");
        embed_resource::compile("assets/recordthis.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed Windows resources");
    }
}
