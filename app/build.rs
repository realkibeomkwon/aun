// Windows: the exe's icon and version information (Explorer, Task Manager, Properties > Details).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/aun.ico").set("ProductName", "Aun").set("FileDescription", "Aun");
        res.compile().expect("compiling the Windows resource");
    }
}
