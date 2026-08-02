//! davimci binary entrypoint.

fn main() -> anyhow::Result<()> {
    println!(
        "davimci {} - pre-implementation scaffold",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "timeline default: {}",
        davimci_core::TimelineProps::default()
    );
    Ok(())
}
