//! vimci binary entrypoint.

fn main() -> anyhow::Result<()> {
    println!(
        "vimci {} - pre-implementation scaffold",
        env!("CARGO_PKG_VERSION")
    );
    println!("timeline default: {}", vimci_core::TimelineProps::default());
    Ok(())
}
