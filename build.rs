use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=frontend/dist");
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/index.html");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");

    let frontend_index = Path::new("frontend/dist/index.html");
    if !frontend_index.exists() {
        panic!(
            "Missing built frontend at frontend/dist/index.html. Run `npm run build` in the frontend directory before building Nagare."
        );
    }
}
