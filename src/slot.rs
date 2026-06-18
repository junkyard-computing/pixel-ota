// A/B slot resolution for Pixel-on-Linux.
//
// The *current* slot is whatever the bootloader booted, exposed to userspace as
// `androidboot.slot_suffix` in /proc/bootconfig (and /proc/cmdline). Slotted partitions live at
// /dev/disk/by-partlabel/<name>_<a|b>.

use std::io;
use std::path::PathBuf;

const BOOTCONFIG: &str = "/proc/bootconfig";
const CMDLINE: &str = "/proc/cmdline";
const BY_PARTLABEL: &str = "/dev/disk/by-partlabel";

/// Slot suffix letter for an index (0 -> "a", 1 -> "b").
pub fn letter(slot: usize) -> char {
    if slot == 0 { 'a' } else { 'b' }
}

/// The other slot.
pub fn inactive(slot: usize) -> usize {
    (slot + 1) % 2
}

/// `/dev/disk/by-partlabel/<name>_<slot>`.
pub fn partition_path(name: &str, slot: usize) -> PathBuf {
    PathBuf::from(BY_PARTLABEL).join(format!("{name}_{}", letter(slot)))
}

/// Extract the active slot index from a bootconfig/cmdline string containing
/// `androidboot.slot_suffix = "_a"` (bootconfig) or `androidboot.slot_suffix=_a` (cmdline).
pub fn parse_slot_suffix(s: &str) -> Option<usize> {
    const KEY: &str = "slot_suffix";
    let idx = s.find(KEY)?;
    // Search AFTER the key (the key itself contains a '_') for the first _a / _b token.
    let after = &s[idx + KEY.len()..];
    let pos = after.find('_')?;
    match after.as_bytes().get(pos + 1) {
        Some(b'a') => Some(0),
        Some(b'b') => Some(1),
        _ => None,
    }
}

/// Read the current (running) slot from the kernel.
pub fn current() -> io::Result<usize> {
    for path in [BOOTCONFIG, CMDLINE] {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Some(slot) = parse_slot_suffix(&s)
        {
            return Ok(slot);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not determine current slot from androidboot.slot_suffix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_and_inactive() {
        assert_eq!(letter(0), 'a');
        assert_eq!(letter(1), 'b');
        assert_eq!(inactive(0), 1);
        assert_eq!(inactive(1), 0);
    }

    #[test]
    fn partition_paths() {
        assert_eq!(
            partition_path("boot", 1),
            PathBuf::from("/dev/disk/by-partlabel/boot_b")
        );
        assert_eq!(
            partition_path("vendor_boot", 0),
            PathBuf::from("/dev/disk/by-partlabel/vendor_boot_a")
        );
    }

    #[test]
    fn parses_bootconfig_form() {
        assert_eq!(
            parse_slot_suffix("androidboot.slot_suffix = \"_a\"\n"),
            Some(0)
        );
        assert_eq!(
            parse_slot_suffix("x\nandroidboot.slot_suffix = \"_b\"\ny"),
            Some(1)
        );
    }

    #[test]
    fn parses_cmdline_form() {
        assert_eq!(
            parse_slot_suffix("console=ttynull androidboot.slot_suffix=_b root=/dev/x"),
            Some(1)
        );
    }

    #[test]
    fn none_when_absent_or_bad() {
        assert_eq!(parse_slot_suffix("no suffix here"), None);
        assert_eq!(parse_slot_suffix("slot_suffix = \"_c\""), None);
    }
}
