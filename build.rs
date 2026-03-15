use std::env;
use std::process::{Command, Stdio};

use anyhow::bail;

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // allow unresolved symbols (resolved by nginx at runtime)
        // NOTE: only required on macos, Linux allows this by default
        println!("cargo:rustc-cdylib-link-arg=-Wl,-undefined,dynamic_lookup");
    }

    println!("cargo::rerun-if-env-changed=DEP_NGINX_OS_CHECK");
    println!(
        "cargo::rustc-check-cfg=cfg(ngx_os, values({}))",
        std::env::var("DEP_NGINX_OS_CHECK").unwrap_or("any()".to_string())
    );
    println!("cargo::rerun-if-env-changed=DEP_NGINX_OS");
    if let Ok(os) = env::var("DEP_NGINX_OS") {
        println!("cargo::rustc-cfg=ngx_os=\"{os}\"");
    }

    let include = env::var("DEP_NGINX_INCLUDE").expect("DEP_NGINX_INCLUDE");
    cc::Build::new()
        .file("src/ffi/expand.c")
        .includes(env::split_paths(&include))
        .compile("expand");

    readme()?;

    Ok(())
}

fn readme() -> anyhow::Result<()> {
    println!("cargo::rerun-if-env-changed=CARGO_PKG_VERSION");
    println!("cargo::rerun-if-changed=README.md.tpl");

    // xtask is not included in the published crate tarball
    if !std::path::Path::new("xtask/Cargo.toml").exists() {
        return Ok(());
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("xtask");
    cmd.arg("readme");

    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let rc = cmd.status()?;
    if !rc.success() {
        bail!("cargo xtask readme failed with {rc}");
    }

    Ok(())
}
