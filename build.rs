use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
extern crate winres;

#[allow(dead_code)]
fn pack_dir_recursive(dir: &Path, bin_file: &mut File, offset: &mut usize, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|de| de.path())).collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();

    let mut code = String::from("vec![\n");
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.starts_with('.') => n,
            _ => continue,
        };
        if path.is_dir() {
            let sub_vec = pack_dir_recursive(&path, bin_file, offset, indent + 4);
            code.push_str(&format!(
                "{}    FSNode::Directory({:?}, {}),\n",
                pad, name, sub_vec
            ));
        } else if path.is_file() {
            let mut data = Vec::new();
            if let Ok(mut f) = File::open(&path) {
                if f.read_to_end(&mut data).is_ok() {
                    let start = *offset;
                    let len = data.len();
                    bin_file.write_all(&data).expect("Failed to write to gamedata.bin");
                    *offset += len;
                    code.push_str(&format!(
                        "{}    FSNode::File({:?}, &EMBEDDED_GAMEDATA_BIN[{}..{}]),\n",
                        pad, name, start, start + len
                    ));
                }
            }
        }
    }
    code.push_str(&format!("{}]", pad));
    code
}

fn generate_embedded_gamedata() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let data_dir = env::var("CAVESTORY_EMBED_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("../gamedata/data"));

    println!("cargo:rerun-if-env-changed=CAVESTORY_EMBED_DATA_DIR");
    println!("cargo:rerun-if-changed={}", data_dir.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bin_path = out_dir.join("gamedata.bin");
    let rs_path = out_dir.join("embedded_gamedata.rs");

    let mut bin_file = File::create(&bin_path).expect("Failed to create gamedata.bin");
    let mut offset = 0usize;

    let tree_code = if data_dir.is_dir() {
        pack_dir_recursive(&data_dir, &mut bin_file, &mut offset, 4)
    } else {
        String::from("Vec::new()")
    };

    let rs_contents = format!(
        "static EMBEDDED_GAMEDATA_BIN: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/gamedata.bin\"));\n\
         fn embedded_gamedata_nodes() -> Vec<FSNode> {{\n    {}\n}}\n",
        tree_code
    );
    fs::write(&rs_path, rs_contents).expect("Failed to write embedded_gamedata.rs");
}

fn main() {
    let target = env::var("TARGET").unwrap_or_else(|e| panic!("{}", e));

    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_FEATURE_BACKEND_MPC").is_ok() {
        generate_embedded_gamedata();
    }

    #[cfg(target_os = "windows")]
    if target.contains("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("res/crabsue-icon.ico");
        res.compile().unwrap();

        if target.contains("i686") {
            // hack
            println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");
            println!("cargo:rustc-link-lib=shlwapi");
        }
    }

    if target.contains("darwin") {
        println!("cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET=10.15");
        println!("cargo:rustc-link-arg=-weak_framework");
        println!("cargo:rustc-link-arg=GameController");
        println!("cargo:rustc-link-arg=-weak_framework");
        println!("cargo:rustc-link-arg=CoreHaptics");
    }

    if target.contains("android") {
        println!("cargo:rustc-link-lib=dylib=GLESv2");
        println!("cargo:rustc-link-lib=dylib=EGL");
    }
}
