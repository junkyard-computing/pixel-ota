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
3. Calls `pixel-bootctl set-active-slot <target>` to switch.
4. Reboot to apply; run a health check + `pixel-bootctl mark-boot-successful` afterwards.

The boot chain it flashes (the slot must be consistent for the bootloader to accept it):
`boot, init_boot, vendor_boot, vendor_kernel_boot, dtbo, vbmeta, vbmeta_system, vbmeta_vendor,
pvmfw`.

### rootfs is out of scope (on purpose)

On these devices `super` (the rootfs) is a **single, non-slotted** partition that is also the
live root — there is no inactive rootfs slot to write. So `pixel-ota` does **not** touch it;
rootfs A/B is a separate concern (software-defined rootfs A/B / kexec). See `PLAN.md`.

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

Early. Boot-chain A/B flashing + slot switch implemented and unit-tested; rootfs A/B,
anti-rollback finalize (`mark-boot-successful`), and remote image fetch are TODO.

## License

Apache-2.0.
