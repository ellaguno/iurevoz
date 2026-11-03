//! En Windows mete el ícono (y la versión) en iurevoz.exe: lo usan el Explorador, la barra de
//! tareas y los accesos del instalador.

fn main() {
    println!("cargo:rerun-if-changed=packaging/icono/iurevoz.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/icono/iurevoz.ico").set("ProductName", "IureVoz").set("FileDescription", "IureVoz");
        res.compile().expect("recursos de Windows (ícono)");
    }
}
