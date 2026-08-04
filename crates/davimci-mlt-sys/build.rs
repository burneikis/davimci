//! Locate libmlt and emit link flags.
//!
//! MLT is LGPL-2.1 and davimci is GPL-3.0, so the link is dynamic and
//! `melt`/`melted` are never vendored. `pkg-config` is asked for
//! the shared library only; a static link here would be a licence defect, not
//! a build preference.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MLT_PKGCONFIG");

    // MLT 7 suffixes its pkg-config name with the major version.
    let names = std::env::var("MLT_PKGCONFIG")
        .map(|n| vec![n])
        .unwrap_or_else(|_| vec!["mlt-framework-7".into(), "mlt-framework".into()]);

    let mut last_err = None;
    for name in &names {
        match pkg_config::Config::new()
            .statik(false)
            .atleast_version("7.0.0")
            .probe(name)
        {
            Ok(_) => return,
            Err(e) => last_err = Some(e),
        }
    }

    // A build script reports failure by printing and exiting; the workspace
    // denies `panic!` even here.
    println!(
        "cargo:warning=libmlt (>= 7.0) not found via pkg-config {names:?}. \
         Install it (Arch: pacman -S mlt, Debian: apt install libmlt-dev) \
         or set MLT_PKGCONFIG. Last error: {last_err:?}"
    );
    std::process::exit(1);
}
