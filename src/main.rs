// SPDX-License-Identifier: Apache-2.0
//
// pixel-ota: online A/B updates for a Pixel (Tensor) running Linux — the userspace analog of
// Android's update_engine. Flashes the inactive slot's boot chain from local images, then
// switches to it via `pixel-bootctl` (the boot_control primitive). No fastboot, no host PC.
//
// Scope: kernel/boot-image A/B (boot, vendor_boot, dtbo, vbmeta*, pvmfw, ...). rootfs (`super`)
// is a single non-slotted partition = the live root, so rootfs A/B is handled separately.

mod bootctl;
mod flash;
mod slot;
mod update;

use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pixel-ota", about = "Online A/B updates for Pixel-on-Linux")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the current slot and the inactive target slot.
    Status,
    /// Flash the inactive slot's boot chain from an image directory, then switch to it.
    Update {
        /// Directory containing <partition>.img files (boot.img, vendor_boot.img, ...).
        image_dir: PathBuf,
        /// Override the target slot (default: the inactive slot).
        #[arg(long)]
        slot: Option<char>,
        /// Validate and report without writing or switching.
        #[arg(long)]
        dry_run: bool,
        /// Flash the inactive slot but do not change the active slot.
        #[arg(long)]
        no_switch: bool,
    },
}

fn target_slot(current: usize, override_letter: Option<char>) -> io::Result<usize> {
    match override_letter {
        None => Ok(slot::inactive(current)),
        Some(c) => match c.to_ascii_lowercase() {
            'a' => Ok(0),
            'b' => Ok(1),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slot must be a or b",
            )),
        },
    }
}

fn cmd_status() -> io::Result<()> {
    let current = slot::current()?;
    println!(
        "current slot:  {}",
        slot::letter(current).to_ascii_uppercase()
    );
    println!(
        "inactive slot: {}",
        slot::letter(slot::inactive(current)).to_ascii_uppercase()
    );
    Ok(())
}

fn cmd_update(
    dir: &std::path::Path,
    slot_override: Option<char>,
    dry_run: bool,
    no_switch: bool,
) -> io::Result<()> {
    let current = slot::current()?;
    let target = target_slot(current, slot_override)?;
    let plan = update::plan(dir, current, target)?;
    update::run(&plan, dry_run, no_switch)
}

fn main() -> io::Result<()> {
    match Args::parse().cmd {
        Cmd::Status => cmd_status()?,
        Cmd::Update {
            image_dir,
            slot,
            dry_run,
            no_switch,
        } => {
            cmd_update(&image_dir, slot, dry_run, no_switch)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_defaults_to_inactive() {
        assert_eq!(target_slot(0, None).unwrap(), 1);
        assert_eq!(target_slot(1, None).unwrap(), 0);
    }

    #[test]
    fn target_override() {
        assert_eq!(target_slot(0, Some('A')).unwrap(), 0);
        assert_eq!(target_slot(0, Some('b')).unwrap(), 1);
        assert!(target_slot(0, Some('z')).is_err());
    }
}
