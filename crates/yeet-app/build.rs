fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("cupertino".into());
    slint_build::compile_with_config("ui/app.slint", config).unwrap();

    // The .exe icon in Explorer and the taskbar. assets/yeet.ico is rendered from ui/logo.svg.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/yeet.ico");
        winresource::WindowsResource::new().set_icon("assets/yeet.ico").compile().unwrap();
    }
}
