use std::{env, fs, path::PathBuf};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // Loader entrypoints return pointers to our own hooks, not preempting
        // libvulkan/SDL/GL symbols already present in the game's link map.
        println!("cargo:rustc-cdylib-link-arg=-Wl,-Bsymbolic-functions");
    }
    println!("cargo:rerun-if-changed=shaders/overlay.vert.glsl");
    println!("cargo:rerun-if-changed=shaders/overlay.frag.glsl");
    println!("cargo:rerun-if-changed=VkLayer_vosulay_overlay.json");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest = fs::read_to_string("VkLayer_vosulay_overlay.json").expect("layer manifest");
    fs::write(out.join("VkLayer_vosulay_overlay.json"), manifest).expect("copy layer manifest");
    for (name, src, stage) in [
        (
            "overlay.vert",
            "shaders/overlay.vert.glsl",
            naga::ShaderStage::Vertex,
        ),
        (
            "overlay.frag",
            "shaders/overlay.frag.glsl",
            naga::ShaderStage::Fragment,
        ),
    ] {
        let text = fs::read_to_string(src).expect("shader source");
        let options = naga::front::glsl::Options {
            stage,
            defines: Default::default(),
        };
        let module = naga::front::glsl::Frontend::default()
            .parse(&options, &text)
            .expect("valid GLSL");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("valid shader");
        let mut options = naga::back::spv::Options::default();
        options
            .flags
            .remove(naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE);
        let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V");
        fs::write(out.join(format!("{name}.rs")), format!("&{words:?}"))
            .expect("write embedded SPIR-V words");
        let bytes: Vec<u8> = words.iter().copied().flat_map(u32::to_le_bytes).collect();
        fs::write(out.join(format!("{name}.spv")), bytes).expect("write SPIR-V");
    }
}
