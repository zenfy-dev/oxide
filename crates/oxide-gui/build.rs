//! Embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/oxide.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/oxide.ico");
        res.set("FileDescription", "Oxide archive manager");
        res.set("ProductName", "Oxide");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
