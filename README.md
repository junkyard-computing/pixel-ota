# pixel-ota

Online A/B updates for a **Google Pixel (Tensor) running Linux** — the userspace analog of
Android's `update_engine`, for fleets of repurposed Pixels running plain Debian.

It flashes the **inactive** slot's boot chain from a directory of images, then switches the
active slot via [`pixel-bootctl`](https://github.com/junkyard-computing/pixel-bootctl) (the
`bootctl` / boot_control primitive). No fastboot, no host PC, no keys — push it over SSH and
update the whole fleet.

```
update_engine   ->  pixel-ota        (this repo: flash inactive slot, orchestrate, rollback)
bootctl / HAL   ->  pixel-bootctl    (the slot-switch primitive: UFS boot LUN + devinfo)
```

## What it does

`pixel-ota update <dir>`:
1. Determines the current slot (from `androidboot.slot_suffix`) and targets the **inactive** one.
2. For each `<partition>.img` in `<dir>` that is a known boot-chain partition, writes it to
   `/dev/disk/by-partlabel/<partition>_<target>` after checking it fits. **It refuses to flash
   the active slot.**
3. Calls `pixel-bootctl set-active-slot <target>` to switch. This is **rollback-safe**: the target
   is marked *active* but **not** *successful*, so if it never boots the bootloader burns its retry
   budget and falls back to the previous slot.
4. Reboot to apply. After a confirmed-good boot, **`pixel-ota confirm`** (→ `pixel-bootctl
   mark-successful`) commits the slot — on the device a post-boot service does this automatically.
   Until committed, a failed boot rolls back.

The boot chain it flashes (the slot must be consistent for the bootloader to accept it):
`boot, init_boot, vendor_boot, vendor_kernel_boot, dtbo, vbmeta, vbmeta_system, vbmeta_vendor,
pvmfw`.

### rootfs: in-place online reflash (no A/B yet)

On these devices `super` (the rootfs) is a **single, non-slotted** partition that is also the
live root — there is no inactive rootfs slot to write. `pixel-ota flash-rootfs <image>` reflashes
it **in place**, online, by riding systemd's *shutdown initramfs*: it stages a static busybox +
the image + a `shutdown` script into `/run/initramfs`, then reboots. At the end of that reboot
systemd pivots into `/run/initramfs` (the supported hook for unmounting root) and the script
`dd`s the image onto `super` once it is no longer mounted.

```sh
pixel-ota flash-rootfs rootfs.img --dry-run    # validate + print the generated shutdown script
pixel-ota flash-rootfs rootfs.img              # stage, arm, reboot to flash
pixel-ota flash-rootfs rootfs.img --no-reboot  # arm only; flashes on your next reboot
```

This is **destructive and rollback-free** — a bad image bricks the root and needs
fastboot/recovery. v1 stages the image in **RAM** (tmpfs), so it must fit a RAM budget
(`--ram-budget-pct`, default 60%). The rollback-capable design — an inactive rootfs target
(`rootfs_a`/`rootfs_b` carved from `userdata`, software A/B with initramfs selection) — is the
"do it right" follow-up. See `PLAN.md`.

## Usage

```sh
pixel-ota status
pixel-ota update /path/to/images --dry-run     # validate + show the plan
pixel-ota update /path/to/images               # flash inactive slot + switch
sudo reboot
```

Flags: `--slot <a|b>` (override target), `--no-switch` (flash only), `--dry-run`.
Run as root (block devices). Requires `pixel-bootctl` on `PATH` for the slot switch.

## Building

Host needs only Nix (flakes). The flake cross-compiles a static
`aarch64-unknown-linux-musl` binary that runs on the device's Debian as-is.

```sh
nix build              # -> result/bin/pixel-ota (static aarch64)
scp result/bin/pixel-ota <device>:/usr/local/bin/

nix develop            # cargo / rustc / clippy / rustfmt dev shell
cargo test
```

## Status

Early. Boot-chain A/B flashing, rollback-safe slot switch, and `confirm` finalize
(`mark-successful`) implemented and unit-tested; btrfs rootfs A/B and remote image fetch are TODO.

## License

Apache-2.0.
