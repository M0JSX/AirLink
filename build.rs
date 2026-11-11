// Embeds the icon and version info into airlink.exe on Windows. No-op elsewhere.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/icons/airlink.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/icons/airlink.ico");
        res.set("ProductName", "AirLink");
        res.set("FileDescription", "AirLink IP audio link");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows icon: {e}");
        }
    }
}
