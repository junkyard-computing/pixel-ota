// Online reflash of the live, single-slot rootfs (`super`) — the "Option A" RAM-pivot path.
//
// `super` (felix sda30, ext4, the mounted `/`) is a single, non-slotted partition, so we cannot
// `dd` over it while it is the live root. Instead we use systemd's *shutdown initramfs* hook: if
// `/run/initramfs/shutdown` exists, systemd pivots into `/run/initramfs` at the very end of a
// reboot and execs that binary — specifically so it can unmount the root it could not unmount
// while running. We populate `/run/initramfs` with a static busybox + a `shutdown` script that,
// once `super` is unmounted, `dd`s the image onto its real device node and reboots.
//
// This is intentionally the crude, no-rollback version: a single live partition, flashed in
// place. A failed write needs fastboot/recovery to recover. The rollback-capable design is a
// btrfs rootfs with A/B subvolumes — see PLAN.md / issue #1, future work.
//
// Two staging sources for the image:
//   * RAM (default): the image is copied into `/run/initramfs` (tmpfs), so it must fit a RAM
//     budget. Simple, but bounded by memory.
//   * Staged (`--staged`): the image already lives on a *persistent* partition (e.g. userdata).
//     We bake a mount of that partition into the `shutdown` script and `dd` straight from it —
//     no RAM copy, so it handles full-partition images. Used for felix's ~8 GB rootfs.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::flash;

/// Default device for the live rootfs partition.
pub const ROOT_DEV: &str = "/dev/disk/by-partlabel/super";
/// systemd looks here for a `shutdown` binary to pivot into at the end of shutdown/reboot.
const INITRAMFS_DIR: &str = "/run/initramfs";
/// Static busybox to stage into the initramfs (provides sh/dd/mount/sync/reboot as applets).
pub const BUSYBOX_DEFAULT: &str = "/bin/busybox";
/// Name of the RAM-staged image inside the initramfs (becomes `/rootfs.img` after the pivot).
const IMAGE_NAME: &str = "rootfs.img";

/// Where the image comes from after the pivot.
#[derive(Debug, Clone)]
pub enum Source {
    /// Copied into the initramfs; read from `/rootfs.img` after the pivot.
    Ram,
    /// Read by mounting a persistent partition fresh in the shutdown initramfs.
    Staged(Backing),
}

/// The persistent partition an `--staged` image lives on, resolved at arm time so the shutdown
/// initramfs (fresh devtmpfs, no by-partlabel symlinks, nothing mounted) can re-mount it by node.
#[derive(Debug, Clone)]
pub struct Backing {
    /// Canonical device node, e.g. `/dev/sda31`.
    pub node: PathBuf,
    /// Filesystem type for the explicit `mount -t`, e.g. `ext4`.
    pub fstype: String,
    /// The image's path *within* that filesystem, e.g. `/rootfs.img`.
    pub fs_path: String,
}

/// Total physical RAM in bytes, from /proc/meminfo `MemTotal` (kB).
pub fn mem_total_bytes() -> io::Result<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo")?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:")
            && let Some(kb) = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
        {
            return Ok(kb * 1024);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "MemTotal not found in /proc/meminfo",
    ))
}

/// RAM staging budget: `pct` percent of total RAM. The image is held in tmpfs alongside the
/// running system, so we keep well clear of MemTotal to avoid OOM during the pivot.
pub fn ram_budget(mem_total: u64, pct: u8) -> u64 {
    mem_total / 100 * pct as u64
}

/// Resolve a partition path (possibly a `/dev/disk/by-partlabel/*` symlink) to its real device
/// node (e.g. `/dev/sda30`). The pivoted initramfs has a fresh devtmpfs with no by-partlabel
/// symlinks, so the `shutdown` script must target the canonical node, baked in at arm time.
pub fn resolve_node(dev: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(dev)
}

/// Parse a `/proc/self/mountinfo` body and resolve the partition backing `target`: the mount
/// whose mount point is the longest prefix of `target`. Returns (node, fstype, fs-relative path
/// of `target`). Pure (takes the file contents) so it can be unit-tested.
pub fn backing_from_mountinfo(
    mountinfo: &str,
    target: &Path,
) -> io::Result<(String, String, String)> {
    let mut best: Option<(&str, &str, &str, &str)> = None; // (mountpoint, root, fstype, source)
    for line in mountinfo.lines() {
        // `<id> <pid> <maj:min> <root> <mountpoint> <opts> <tags...> - <fstype> <source> <super>`
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let lf: Vec<&str> = left.split_whitespace().collect();
        let rf: Vec<&str> = right.split_whitespace().collect();
        if lf.len() < 5 || rf.len() < 2 {
            continue;
        }
        let (root, mountpoint, fstype, source) = (lf[3], lf[4], rf[0], rf[1]);
        if target.starts_with(mountpoint)
            && best.is_none_or(|(mp, ..)| mountpoint.len() >= mp.len())
        {
            best = Some((mountpoint, root, fstype, source));
        }
    }
    let (mountpoint, root, fstype, source) = best.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no backing mount found for {}", target.display()),
        )
    })?;
    let rel = target.strip_prefix(mountpoint).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "target not under its mount point",
        )
    })?;
    let fs_path = Path::new(root).join(rel).to_string_lossy().into_owned();
    Ok((source.to_string(), fstype.to_string(), fs_path))
}

/// Resolve the persistent partition an already-staged `image` lives on (via /proc/self/mountinfo).
pub fn backing_mount(image: &Path) -> io::Result<Backing> {
    let target = fs::canonicalize(image)?;
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let (source, fstype, fs_path) = backing_from_mountinfo(&mountinfo, &target)?;
    let node = fs::canonicalize(&source).unwrap_or_else(|_| PathBuf::from(&source));
    Ok(Backing {
        node,
        fstype,
        fs_path,
    })
}

/// The `shutdown` script that runs *after* `super` is unmounted, inside the pivoted initramfs
/// (so `/busybox` is the staged binary; `/oldroot` is the old root tree). Pure / deterministic so
/// it can be unit-tested.
pub fn shutdown_script(real_node: &str, src: &Source) -> String {
    let (prep, infile, log) = match src {
        Source::Ram => (
            String::new(),
            format!("/{IMAGE_NAME}"),
            // RAM is wiped on reboot; kmsg is the only postmortem.
            String::new(),
        ),
        Source::Staged(b) => (
            format!(
                "$BB mkdir -p /stage\n\
                 $BB mount -t {fstype} -o ro {node} /stage\n",
                fstype = b.fstype,
                node = b.node.display(),
            ),
            format!("/stage{}", b.fs_path),
            // Persistent log back onto the staging partition.
            "$BB mount -o remount,rw /stage 2>/dev/null\n\
             echo \"pixel-ota flash-rootfs: dd rc=$RC ($($BB date 2>/dev/null))\" >> /stage/pixel-ota-flash.log 2>/dev/null\n\
             $BB umount /stage 2>/dev/null\n"
                .to_string(),
        ),
    };
    format!(
        "#!/busybox sh\n\
         # pixel-ota: flash the live rootfs after systemd pivoted into the shutdown initramfs.\n\
         BB=/busybox\n\
         $BB mount -t devtmpfs dev /dev 2>/dev/null\n\
         $BB mount -t proc proc /proc 2>/dev/null\n\
         # release the old root tree so {real_node} is no longer busy, then flash it.\n\
         $BB umount -r -l /oldroot 2>/dev/null\n\
         {prep}\
         $BB dd if={infile} of={real_node} bs=4M conv=fsync\n\
         RC=$?\n\
         $BB sync\n\
         echo \"pixel-ota flash-rootfs: dd rc=$RC\" > /dev/kmsg 2>/dev/null\n\
         {log}\
         $BB sleep 2\n\
         $BB reboot -f\n",
    )
}

#[derive(Debug)]
pub struct Plan {
    pub image: PathBuf,
    pub image_len: u64,
    pub root_dev: PathBuf,
    pub real_node: PathBuf,
    pub busybox: PathBuf,
    pub source: Source,
    /// RAM budget (0 in staged mode, which is not RAM-bounded).
    pub budget: u64,
}

/// Validate the request and resolve everything the arm step needs, without touching the system.
pub fn plan(
    image: &Path,
    root_dev: &Path,
    busybox: &Path,
    ram_budget_pct: u8,
    staged: bool,
) -> io::Result<Plan> {
    let image_len = flash::device_size(image)?; // seek-to-end; works for files too
    let part_len = flash::device_size(root_dev)?;
    if !flash::fits(image_len, part_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "image ({image_len} bytes) does not fit {} ({part_len} bytes)",
                root_dev.display()
            ),
        ));
    }
    if !busybox.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("busybox not found at {}", busybox.display()),
        ));
    }
    let real_node = resolve_node(root_dev)?;

    let (source, budget) = if staged {
        let backing = backing_mount(image)?;
        if backing.node == real_node {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "staged image is on the target partition ({}); stage it on a different \
                     partition (e.g. userdata)",
                    real_node.display()
                ),
            ));
        }
        (Source::Staged(backing), 0)
    } else {
        let budget = ram_budget(mem_total_bytes()?, ram_budget_pct);
        if image_len > budget {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "image ({image_len} bytes) exceeds the RAM staging budget ({budget} bytes, \
                     {ram_budget_pct}% of RAM); use --staged to flash from a persistent partition"
                ),
            ));
        }
        (Source::Ram, budget)
    };

    Ok(Plan {
        image: image.to_path_buf(),
        image_len,
        root_dev: root_dev.to_path_buf(),
        real_node,
        busybox: busybox.to_path_buf(),
        source,
        budget,
    })
}

/// Stage busybox + the `shutdown` script (and, in RAM mode, the image) into `/run/initramfs`,
/// arming the flash for the next reboot. A fresh `/run` (tmpfs) clears this on every boot, so
/// arming only ever affects the *next* shutdown.
fn arm(p: &Plan) -> io::Result<()> {
    let dir = Path::new(INITRAMFS_DIR);
    fs::create_dir_all(dir)?;

    let bb = dir.join("busybox");
    fs::copy(&p.busybox, &bb)?;
    fs::set_permissions(&bb, fs::Permissions::from_mode(0o755))?;

    if matches!(p.source, Source::Ram) {
        fs::copy(&p.image, dir.join(IMAGE_NAME))?;
    }

    // systemd execs exactly `/run/initramfs/shutdown`; it must be executable.
    let script = dir.join("shutdown");
    fs::write(
        &script,
        shutdown_script(&p.real_node.to_string_lossy(), &p.source),
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// `flash-rootfs`: validate, (unless dry-run) arm `/run/initramfs`, then (unless no-reboot)
/// trigger the reboot that performs the flash.
pub fn run(
    image: &Path,
    root_dev: &Path,
    busybox: &Path,
    ram_budget_pct: u8,
    staged: bool,
    dry_run: bool,
    no_reboot: bool,
) -> io::Result<()> {
    let p = plan(image, root_dev, busybox, ram_budget_pct, staged)?;
    println!(
        "flash-rootfs: {} ({} bytes) -> {} (node {}){}",
        p.image.display(),
        p.image_len,
        p.root_dev.display(),
        p.real_node.display(),
        if dry_run { " [dry-run]" } else { "" }
    );
    match &p.source {
        Source::Ram => println!(
            "  staging in RAM ({} of {} byte budget), via systemd shutdown initramfs at {}",
            p.image_len, p.budget, INITRAMFS_DIR
        ),
        Source::Staged(b) => println!(
            "  staged on {} ({}), mounted + flashed from the shutdown initramfs at {}",
            b.node.display(),
            b.fstype,
            INITRAMFS_DIR
        ),
    }
    if dry_run {
        println!("--- {INITRAMFS_DIR}/shutdown ---");
        print!(
            "{}",
            shutdown_script(&p.real_node.to_string_lossy(), &p.source)
        );
        println!(
            "would copy busybox {} and arm, then reboot",
            p.busybox.display()
        );
        return Ok(());
    }

    arm(&p)?;
    println!("armed {INITRAMFS_DIR} (busybox + shutdown script).");
    println!(
        "WARNING: the next reboot will overwrite {} in place. No rollback — a bad image needs \
         fastboot/recovery to recover.",
        p.real_node.display()
    );
    if no_reboot {
        println!("--no-reboot: the flash runs on your next `reboot`.");
        return Ok(());
    }
    println!("rebooting now to flash...");
    let status = Command::new("systemctl").arg("reboot").status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "`systemctl reboot` exited with {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_percent_of_ram() {
        assert_eq!(ram_budget(10_000, 60), 6_000);
        assert_eq!(ram_budget(0, 60), 0);
    }

    #[test]
    fn ram_script_targets_real_node_and_staged_image() {
        let s = shutdown_script("/dev/sda30", &Source::Ram);
        assert!(s.starts_with("#!/busybox sh\n"));
        assert!(s.contains("dd if=/rootfs.img of=/dev/sda30 bs=4M conv=fsync"));
        assert!(s.contains("umount -r -l /oldroot"));
        assert!(s.contains("reboot -f"));
        assert!(!s.contains("/stage"));
    }

    #[test]
    fn staged_script_mounts_partition_and_dds_from_it() {
        let src = Source::Staged(Backing {
            node: PathBuf::from("/dev/sda31"),
            fstype: "ext4".to_string(),
            fs_path: "/rootfs.img".to_string(),
        });
        let s = shutdown_script("/dev/sda30", &src);
        assert!(s.contains("mount -t ext4 -o ro /dev/sda31 /stage"));
        assert!(s.contains("dd if=/stage/rootfs.img of=/dev/sda30 bs=4M conv=fsync"));
        assert!(s.contains("pixel-ota-flash.log"));
    }

    #[test]
    fn backing_resolves_longest_prefix_mount() {
        // userdata mounted at /mnt/ud (root "/"); root fs at "/".
        let mi = "23 1 8:30 / / rw,relatime shared:1 - ext4 /dev/sda30 rw\n\
                  44 23 8:31 / /mnt/ud rw,relatime shared:2 - ext4 /dev/sda31 rw,stripe=128\n";
        let (src, fstype, fs_path) =
            backing_from_mountinfo(mi, Path::new("/mnt/ud/sub/rootfs.img")).unwrap();
        assert_eq!(src, "/dev/sda31");
        assert_eq!(fstype, "ext4");
        assert_eq!(fs_path, "/sub/rootfs.img");
    }

    #[test]
    fn plan_rejects_image_larger_than_partition() {
        // image = this source file; root_dev = /dev/null (0 bytes) so nothing fits.
        let src = Path::new(file!());
        let err = plan(
            src,
            Path::new("/dev/null"),
            Path::new(BUSYBOX_DEFAULT),
            60,
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
