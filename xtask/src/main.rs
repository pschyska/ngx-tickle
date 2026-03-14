use std::env;
use std::fs::{self};
use std::path::Path;
use std::process::ExitCode;

use serde::Serialize;
use tinytemplate::TinyTemplate;

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("readme") => match readme() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("Usage: cargo xtask <command>");
            eprintln!();
            eprintln!("Commands:");
            eprintln!("  readme    Render README.md.tpl");
            ExitCode::FAILURE
        }
    }
}

#[derive(Serialize)]
struct TemplateData {
    version: String,
}

fn readme() -> anyhow::Result<()> {
    let metadata = cargo_metadata::MetadataCommand::new().exec()?;
    let version = &metadata.root_package().unwrap().version;

    let mut tt = TinyTemplate::new();
    let sauce = Path::new("README.tpl.md");
    let target = Path::new("README.md");

    let tpl = fs::read_to_string(sauce)?;

    tt.add_template(target.to_str().unwrap(), &tpl)?;

    let data = TemplateData {
        version: version.to_string(),
    };

    let new = tt.render(target.to_str().unwrap(), &data)?;

    let current = if target.exists() {
        Some(fs::read_to_string(target)?)
    } else {
        None
    };

    if current.is_none_or(|current| current != new) {
        fs::write(target, new)?;
        eprintln!("{} → {}", sauce.display(), target.display());
    }
    Ok(())
}
