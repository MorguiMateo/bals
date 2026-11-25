fn main() {
    println!("cargo:rerun-if-changed=assets/bals.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/bals.ico");
        res.set("ProductName", "Bals");
        res.set("FileDescription", "Bals — Cliper liviano de videojuegos");
        if let Err(err) = res.compile() {
            panic!("No se pudo compilar el recurso de icono de Windows: {err}");
        }
    }
}
