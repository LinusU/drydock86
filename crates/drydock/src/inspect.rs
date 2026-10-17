use anyhow::{Context, Result};
use drydock_core::pe::PeImage;
use sha2::Digest;
use std::path::Path;

pub fn run(exe: &Path, json: bool) -> Result<()> {
    let data = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
    let pe = PeImage::parse(&data)?;
    let img = pe.map_image(&data);
    let imports = pe.imports(&img).unwrap_or_default();
    let exports = pe.exports(&img).unwrap_or_default();
    let sha = hex::encode(sha2::Sha256::digest(&data));
    if json {
        let v = serde_json::json!({
            "path": exe.display().to_string(),
            "sha256": sha,
            "size": data.len(),
            "image": pe,
            "imports": imports,
            "exports": exports,
            "tls": pe.tls(&img).ok().flatten(),
            "resources": pe.resources(&img).unwrap_or_default().len(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{}", exe.display());
    println!("  sha256      {sha}");
    println!("  kind        {}", if pe.is_dll() { "DLL" } else { "EXE" });
    println!("  timestamp   {:#x}", pe.timestamp);
    println!("  image base  {:#010x}  size {:#x}", pe.image_base, pe.size_of_image);
    println!("  entry       {:#010x}", pe.image_base + pe.entry_rva);
    println!(
        "  subsystem   {} ({}.{})",
        pe.subsystem, pe.major_subsystem, pe.minor_subsystem
    );
    println!(
        "  relocations {}",
        if pe.dir(5).rva != 0 { "present" } else { "stripped" }
    );
    println!("  sections");
    for s in &pe.sections {
        println!(
            "    {:8} va {:#010x} vsize {:#09x} raw {:#09x} flags {:#010x}",
            s.name, s.virtual_address, s.virtual_size, s.raw_size, s.characteristics
        );
    }
    println!("  imports");
    for d in &imports {
        println!("    {} ({} functions)", d.dll, d.entries.len());
    }
    if !exports.entries.is_empty() {
        println!("  exports ({}): {}", exports.entries.len(), exports.dll_name);
    }
    Ok(())
}
