use std::{env, error::Error, path::PathBuf, process::Command};

use vergen_gitcl::{Emitter, Gitcl};

fn main() -> Result<(), Box<dyn Error>> {
    Emitter::default()
        .default_on_error()
        .add_instructions(&Gitcl::builder().branch(true).sha(true).dirty(true).build())?
        .emit()?;

    // An intentionally absent file refreshes metadata on every build, including
    // changes to untracked files that Git's HEAD/index watchers cannot detect.
    let refresh = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?)
        .join("refresh-git-metadata");
    println!("cargo:rerun-if-changed={}", refresh.display());

    // Match the first-parent commit height used by release versioning in CI.
    let number = Command::new("git")
        .args(["rev-list", "--first-parent", "--count", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|number| number.trim().parse::<u64>().ok())
        .map_or_else(|| "unknown".to_owned(), |number| number.to_string());
    println!("cargo:rustc-env=TERMGRAM_BUILD_NUMBER={number}");
    Ok(())
}
