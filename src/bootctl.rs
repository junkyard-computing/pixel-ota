// Slot-switch + rollback are delegated to `pixel-bootctl` (the boot_control primitive), exactly
// as Android's update_engine delegates to bootctl. pixel-ota does NOT reimplement the UFS
// boot-LUN write; it calls the dedicated tool.

use std::io;
use std::process::Command;

use crate::slot;

const BOOTCTL_BIN: &str = "pixel-bootctl";

fn run(args: &[&str]) -> io::Result<()> {
    let status = Command::new(BOOTCTL_BIN).args(args).status().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to run `{BOOTCTL_BIN}` (is it installed/on PATH?): {e}"),
        )
    })?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "`{BOOTCTL_BIN} {}` exited with {status}",
            args.join(" ")
        )));
    }
    Ok(())
}

/// `pixel-bootctl set-active-slot <a|b>`. Rollback-safe: pixel-bootctl marks the slot active but
/// not successful, so a slot that never boots rolls back. Commit it with `mark_successful` after.
pub fn set_active_slot(slot: usize) -> io::Result<()> {
    run(&["set-active-slot", &slot::letter(slot).to_string()])
}

/// `pixel-bootctl mark-successful` — commit the running slot after a confirmed-good boot, so the
/// bootloader stops counting down its retry budget and won't roll back.
pub fn mark_successful() -> io::Result<()> {
    run(&["mark-successful"])
}
