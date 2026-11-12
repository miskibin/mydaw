fn main() {
    println!("cargo::rerun-if-changed=src/wrapper.cpp");
    println!("cargo::rerun-if-changed=vendor");
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .file("src/wrapper.cpp")
        .include("vendor")
        .include("vendor/signalsmith-stretch")
        // the headers are third-party: their warnings are not ours to fix
        .warnings(false)
        .opt_level(3)
        .compile("wave_stretch");
}
