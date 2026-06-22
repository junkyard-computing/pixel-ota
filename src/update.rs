// The OTA orchestration: flash the inactive slot's boot chain from a directory of images, then
// switch to it via pixel-bootctl. This is the kernel/boot-image A/B path; rootfs A/B is separate.

use std::fs;
use std::io;
use std::path::Path;

use crate::{bootctl, flash, slot};

#[derive(Debug)]
pub struct Plan {
    pub current: usize,
    pub target: usize,
    /// (image path, partition name, target device path)
    pub items: Vec<(std::path::PathBuf, &'static str, std::path::PathBuf)>,
}

/// Build the flash plan from an image directory for a given target slot.
/// Refuses to target the currently-running slot.
pub fn plan(image_dir: &Path, current: usize, target: usize) -> io::Result<Plan> {
    if target == current {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to flash the active slot ({}); pixel-ota flashes the inactive slot",
                slot::letter(current)
            ),
        ));
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(image_dir)? {
        let path = entry?.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(part) = flash::image_to_partition(fname) {
            let dev = slot::partition_path(part, target);
            items.push((path, part, dev));
        }
    }
    items.sort_by_key(|(_, part, _)| flash::BOOT_CHAIN.iter().position(|p| p == part));
    Ok(Plan {
        current,
        target,
        items,
    })
}

/// Execute the update: flash the chain, then (unless `no_switch`) set the target slot active.
pub fn run(p: &Plan, dry_run: bool, no_switch: bool) -> io::Result<()> {
    if p.items.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no boot-chain images found in directory",
        ));
    }
    println!(
        "updating slot {} -> {} ({} image(s)){}",
        slot::letter(p.current),
        slot::letter(p.target),
        p.items.len(),
        if dry_run { " [dry-run]" } else { "" }
    );
    for (img, part, dev) in &p.items {
        if !dev.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("target device {} not found", dev.display()),
            ));
        }
        let n = flash::flash_image(img, dev, dry_run)?;
        println!(
            "  {} {} -> {} ({n} bytes)",
            if dry_run { "would flash" } else { "flashed" },
            part,
            dev.display()
        );
    }
    if no_switch {
        println!("--no-switch: leaving active slot unchanged");
        return Ok(());
    }
    if dry_run {
        println!(
            "would set active slot -> {} via pixel-bootctl (rollback-safe: active, not successful)",
            slot::letter(p.target)
        );
        return Ok(());
    }
    bootctl::set_active_slot(p.target)?;
    println!(
        "active slot set to {} (rollback-safe: marked active, NOT successful).",
        slot::letter(p.target)
    );
    println!(
        "reboot to apply. After a confirmed-good boot, run `pixel-ota confirm` (or let the \
         boot-success service) to commit it — otherwise the bootloader rolls back to {}.",
        slot::letter(p.current)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_active_slot() {
        let err = plan(Path::new("/nonexistent"), 0, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
