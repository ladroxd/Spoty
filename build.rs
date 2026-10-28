// Embeds the app icon (and file details) into spoty.exe so Explorer shows it.
fn main() {
    println!("cargo:rerun-if-changed=assets/spoty.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/spoty.ico");
        res.set("FileDescription", "Spoty");
        res.set("ProductName", "Spoty");
        if let Err(e) = res.compile() {
            println!("cargo:warning=couldn't embed the icon: {e}");
        }
    }
}
