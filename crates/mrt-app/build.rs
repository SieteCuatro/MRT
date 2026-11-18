//! Windows: embeds the icon (`assets/mrt.ico`) and the version information shown in
//! Explorer's file properties into `mrt.exe`. Other targets need nothing.

fn main() {
    println!("cargo:rerun-if-changed=assets/mrt.ico");
    println!("cargo:rerun-if-changed=mrt.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut numbers: Vec<u16> = version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .take(3)
        .collect();
    numbers.resize(4, 0);
    let file_version = numbers.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let defines = [format!("MRT_VERSION_NUMBERS={file_version}"), format!("MRT_VERSION_TEXT=\"{version}\"")];
    embed_resource::compile("mrt.rc", defines.iter().map(String::as_str))
        .manifest_optional()
        .expect("could not compile mrt.rc (needs the Windows SDK resource compiler)");
}
