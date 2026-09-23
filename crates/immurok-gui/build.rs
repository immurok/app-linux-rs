fn main() {
    glib_build_tools::compile_resources(
        &["data/icons"],
        "data/immurok.gresource.xml",
        "immurok.gresource",
    );
}
