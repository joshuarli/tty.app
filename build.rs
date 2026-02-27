fn main() {
    // Shader is compiled at runtime from source using device.new_library_with_source().
    // This avoids requiring the Metal offline compiler toolchain at build time.
    println!("cargo:rerun-if-changed=src/renderer/shader.metal");
}
