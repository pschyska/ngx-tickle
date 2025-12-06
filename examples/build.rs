use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;

fn get_nginx_src_path() -> Result<PathBuf> {
    let metadata = MetadataCommand::new().exec().unwrap();
    let nginx_src = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "nginx-src");
    nginx_src
        .map(|p| p.manifest_path.parent().unwrap().join("nginx").into())
        .context("nginx source folder")
}

fn nproc() -> usize {
    Command::new("nproc")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

// install nginx to ./prefix for DX
pub fn make_install() -> Result<()> {
    println!("cargo:rerun-if-env-changed=NGX_CONFIGURE_ARGS");

    println!("cargo:rerun-if-env-changed=MAKE");
    let make = env::var("MAKE").unwrap_or_else(|_| "make".to_string());
    let jobs = env::var("NUM_JOBS").unwrap_or_else(|_| format!("{}", nproc()));

    println!("cargo:rerun-if-env-changed=DEP_NGINX_BUILD_DIR");
    let build_dir = std::env::var("DEP_NGINX_BUILD_DIR").unwrap();
    let build_dir = Path::new(&build_dir);
    let source_dir = get_nginx_src_path()?;

    eprintln!(
        "running `{make} -f {}/Makefile -j{jobs} install` in {}",
        build_dir.display(),
        source_dir.display()
    );

    Command::new(&make)
        .arg("-f")
        .arg(build_dir.join("Makefile"))
        .arg(format!("-j{jobs}"))
        .arg("install")
        .current_dir(&source_dir)
        .status()?;
    Ok(())
}

fn main() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        // allow unresolved symbols (resolved by nginx at runtime)
        // NOTE: only required on macos, Linux allows this by default
        println!("cargo:rustc-cdylib-link-arg=-Wl,-undefined,dynamic_lookup");
    }

    make_install()?;

    Ok(())
}
