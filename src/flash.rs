// Writing images to slotted partition block devices, with verification.
//
// We only ever flash the *inactive* slot's boot chain. rootfs (`super`) is a single,
// non-slotted partition that is also the live root, so it is intentionally NOT in this list —
// rootfs A/B is handled separately (software A/B), not by this updater.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Slotted verified-boot-chain partitions, in flash order. A felix slot switch only boots if the
/// whole chain on the target slot is consistent (we confirmed dtbo/vbmeta*/pvmfw differ per slot).
pub const BOOT_CHAIN: &[&str] = &[
    "boot",
    "init_boot",
    "vendor_boot",
    "vendor_kernel_boot",
    "dtbo",
    "vbmeta",
    "vbmeta_system",
    "vbmeta_vendor",
    "pvmfw",
];

/// Map an image filename (e.g. "vendor_boot.img") to its partition name if it is a known
/// boot-chain partition. Returns None for anything we don't flash (e.g. rootfs/super.img).
pub fn image_to_partition(filename: &str) -> Option<&'static str> {
    let stem = filename.strip_suffix(".img").unwrap_or(filename);
    BOOT_CHAIN.iter().copied().find(|&p| p == stem)
}

/// Does `image_len` fit within a partition of `part_len` bytes?
pub fn fits(image_len: u64, part_len: u64) -> bool {
    image_len <= part_len
}

/// Size of a block device (or file) in bytes via seek-to-end.
pub fn device_size(path: &Path) -> io::Result<u64> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::End(0))
}

/// Write `image` to the block device at `dev`, after checking it fits. With `dry_run`, only
/// validates and reports. Returns the number of bytes that would be / were written.
pub fn flash_image(image: &Path, dev: &Path, dry_run: bool) -> io::Result<u64> {
    let mut src = File::open(image)?;
    let image_len = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    let part_len = device_size(dev)?;
    if !fits(image_len, part_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} ({image_len} bytes) does not fit {} ({part_len} bytes)",
                image.display(),
                dev.display()
            ),
        ));
    }
    if dry_run {
        return Ok(image_len);
    }
    let mut data = Vec::with_capacity(image_len as usize);
    src.read_to_end(&mut data)?;
    let mut dst = OpenOptions::new().write(true).open(dev)?;
    dst.write_all(&data)?;
    dst.sync_all()?;
    Ok(image_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_images() {
        assert_eq!(image_to_partition("boot.img"), Some("boot"));
        assert_eq!(
            image_to_partition("vbmeta_system.img"),
            Some("vbmeta_system")
        );
        assert_eq!(image_to_partition("dtbo"), Some("dtbo"));
    }

    #[test]
    fn ignores_unknown_and_rootfs() {
        assert_eq!(image_to_partition("super.img"), None);
        assert_eq!(image_to_partition("rootfs.img"), None);
        assert_eq!(image_to_partition("README.md"), None);
    }

    #[test]
    fn fits_bounds() {
        assert!(fits(100, 100));
        assert!(fits(0, 64));
        assert!(!fits(101, 100));
    }
}
