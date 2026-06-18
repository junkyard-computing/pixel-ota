# Goal

Flash Android partitions from a running Debian on a Pixel Fold (felix). Configure the
bootloader's A/B boot flags. Replicate OTA — but **online**: write updates to the inactive
slot while the system is running, flip the active slot, and reboot, instead of doing the
update at boot time in the initramfs.

This is, in effect, standard Android A/B OTA behavior reimplemented for a Debian userspace
that has replaced Android: no host PC, no `fastboot`, no boot-time dracut update phase.

# Project identity & architecture (decided 2026-06-18)

We are reimplementing Android's two A/B OTA components for a Pixel running Debian. Name tools by
**capability**, mirroring the Android components, not by the byte-store they happen to poke:

| Android component | Role | Our analog (repo) |
| --- | --- | --- |
| `bootctl` / `boot_control` HAL | slot control: get/set active slot, mark successful, set unbootable | **`pixel-bootctl`** (renamed from `pixel-devinfo`) |
| `update_engine` | applies the A/B update, drives the slot switch, handles rollback, fleet push | **`pixel-ota`** (this repo, renamed from `online-flash`) |

- **`pixel-bootctl`** — the boot-slot primitive. Backends: a `devinfo` module (read/report slot
  state; the bootloader treats devinfo largely as a mirror — see findings) and a **`trusty`
  module** that drives the real `setActiveBootSlot` via the TEE. The original `pixel-devinfo`
  devinfo parser becomes an internal module/crate, accurate for that one file but no longer the
  product identity.
- **`pixel-ota`** (this repo) — the updater on top: flash the inactive targets from Debian,
  drive the slot/rootfs switch via `pixel-bootctl`, handle rollback, push across the fleet over
  SSH. **Supersedes `OTA-Updates`** (which was the team's initial research into techniques — its
  rootfs-A/B ideas migrate in here).

# Context (existing repos)

- **`pixel-devinfo` → `pixel-bootctl`** — Rust tool that parses/edits the `devinfo` partition's
  boot flags (`Devinfo` magic `"DEVI"`, `SlotData{retry_count, unbootable, successful, active,
  fastboot_ok}`, LE bit-packed; deps `byteorder`, `clap`). **Renamed & rescoped** to the
  boot-control capability; devinfo editing alone does NOT switch slots on felix (see findings).
- **`junkyard-boot-img`** — builds the image trio (`boot.img`, `vendor_boot.img`, ext4
  `rootfs.img`) for felix; flashes from a host via `fastboot` + `flash.sh`. `pixel-ota` is the
  from-device replacement for that host flash step.
- **`OTA-Updates`** — *research stage*; boot-time dracut A/B via btrfs subvolumes. **Superseded
  by `pixel-ota`**; salvage its rootfs-A/B approach.

# Slot-switch mechanism — the central finding (see detailed log below)

Editing devinfo does NOT switch the active slot on felix; the bootloader keeps its choice in a
**TEE/RPMB-backed store** and rewrites devinfo to match. The real switch is `setActiveBootSlot`
via the **Trusty TEE** — and slot switching is **keyless** (Google's keys are only for verified
boot / signing). The Trusty transport is **already present under Debian** (`/dev/trusty-ipc-dev0`,
`/dev/gsa0`, `trusty_ipc` loaded, secure world live). So `pixel-bootctl`'s `set-active` is a
small Trusty IPC client; the remaining unknown is the boot_control TA's **port name + message
format** (reverse from Google's HAL). Fallback if that doesn't pan out: **kexec-shim** (kernel
A/B without switching the bootloader slot). Full details, dead-ends, and evidence are in the
sections below (kept as the investigation record).

# `pixel-bootctl` CLI (the bootctl surface)

- `get-current-slot` / `status` — report slot state (devinfo read; later cross-check via Trusty).
- `set-active-slot <a|b>` — **Trusty `setActiveBootSlot`** (the real switch). NOT a devinfo write.
- `mark-boot-successful` — arm rollback (devinfo and/or Trusty — TBD which the bootloader honors).
- `set-unbootable <a|b>` — likewise.

# `pixel-ota` CLI (the update_engine surface)

- `flash <partition> <image> [--slot a|b] [--dry-run]` — write one image to the inactive slot's
  block device (`/dev/disk/by-partlabel/<part>_<slot>`), with size/magic verification; refuse
  the active slot.
- `update <image-dir> [--dry-run] [--reboot]` — orchestrate: flash inactive boot-chain
  partitions + rootfs target, call `pixel-bootctl set-active-slot`, reboot.
- `confirm` — post-boot health gate → `pixel-bootctl mark-boot-successful` (systemd unit).

## Slot & partition resolution  (verified on device, 2026-06-17)

- Read the current active slot from `devinfo`; the target is the other slot.
- Map partition name + slot → block device via **`/dev/disk/by-partlabel/<part>_<slot>`**.
  This felix Debian build has **no** `/dev/block/by-name/` (an Android-only path); the udev
  `by-partlabel` links are present and reliable.
- Verified slotted partitions: `boot_{a,b}` (sda10/sda20), `vendor_boot_{a,b}` (sda12/sda22),
  plus `init_boot`, `vendor_kernel_boot`, `dtbo`, `vbmeta`/`vbmeta_system`/`vbmeta_vendor`,
  `pvmfw`, `modem`, and all bootloaders on sdb/sdc (`bl1`, `bl2`, `abl`, `tzsw`, `gsa`, …).
- Writing = open the target block device, verify size, stream the image, `fsync`,
  re-read to confirm (optional hash check).

### devinfo — confirmed

- `/dev/disk/by-partlabel/devinfo` → `sdd1`, **8192 bytes**, magic `DEVI` at offset 0,
  version bytes `03 00 0f 00`. Matches `pixel-devinfo`'s parser.
- The partition also carries `DIUS`/`DIFR` factory key/value strings (sku `G9FPL`, serial,
  `pcbcfg`, `bootmode`) **after** the devinfo struct — the tool must preserve all bytes
  outside its boot-flag fields (it already does selective writes).
- No `bootctl` or `fastboot` on the device — editing `devinfo` is the only slot-control path,
  confirming `pixel-devinfo`'s central role.

## Slot-switching investigation — CRITICAL ⚠️ (on-device, 2026-06-17)

We built `pixel-devinfo` on the test device (cargo 1.85 from apt) and ran live reboot tests.
**Editing devinfo's per-slot flags does NOT switch the active boot slot on felix.** This
breaks the plan's core assumption that flipping the devinfo `active` bit drives slot selection.

What `pixel-devinfo` does and what we observed:
- The tool reads devinfo correctly (Version 3.15) and, on `-s B`, writes **only** bytes 48–55:
  per-slot `retry_count` (byte 48/52) and a flags byte (49/53: bit0 unbootable, bit1
  successful, bit2 active, bit3 fastboot_ok). Confirmed by byte-diff.
- Across multiple reboots we set B = active + successful + `retry_count=7` and A = `retry_count=0`.
  **Every time the device booted slot A.** The bootloader then *repaired* devinfo: restored
  A to active + retry 7, cleared B's active bit, and — notably — never marked B `unbootable`.
- klog (`/dev/disk/by-partlabel/klog`) shows the decision: `boot slot: a` →
  `AB Decision: active slot boot ok`. **`boot slot: b` never appears** and there is **no
  `vbmeta_b` evaluation** — B is never even considered, regardless of its flags.
- The bootloader is **orange/unlocked**; slot A verifies `OK_NOT_SIGNED` and boots permissively.

Conclusions:
1. **`pixel-devinfo` cannot switch slots as written.** It edits *status* flags, not the
   *selector*. The actual active-slot selector is a field the tool never writes — most likely
   the devinfo region at **`0x24–0x2b`** (`03 00 01 00 03 06 06 01`), which stayed constant
   through every flag edit, or a store outside devinfo. **This must be reverse-engineered.**
2. **Booting a custom inactive slot needs its whole verified-boot chain**, not just
   boot+vendor_boot. On this device `dtbo`, `vbmeta`, `vbmeta_system`, `vbmeta_vendor`, `pvmfw`
   differed between slots; cloning A→B made them match (but selection still never reached B).
3. The `successful` flag and `retry_count` do not override the current slot — the bootloader
   stays on the running/known-good slot. A *legitimate* switch (priority bump the bootloader
   honors) must use whatever field its own `fastboot --set-active` writes.

**RESOLVED (host fastboot experiment, 2026-06-18):** `fastboot --set-active=b` from a host
*did* switch slots (device booted `_b`), and diffing devinfo showed it changed bytes `0x6d–0x6f`
(plus the per-slot flags). We then tried to reproduce that from Debian — wrote those exact
selector bytes, then wrote a **complete pristine bootloader-blessed A-state devinfo** wholesale.
**Both still booted the bootloader's existing choice and the bootloader rewrote devinfo to match.**

**Conclusion — the A/B slot is NOT controllable from userspace.** It lives in a
bootloader-protected store (RPMB-class). The devinfo partition is a *mirror* the bootloader
rewrites each boot to reflect its own decision; writing devinfo from Debian never changes the
slot. Only `fastboot --set-active` (via the bootloader) switches slots. `pixel-devinfo` cannot
switch slots, and no extension of it can — the lever isn't in devinfo.

**Therefore the whole "flip the devinfo active slot" premise is dead for online OTA.** See the
new design below — rootfs is slot-independent (`root=/dev/disk/by-partlabel/super`), so OTA is
done in software without touching the bootloader slot.

Test-device state: currently running **slot B** (B's chain was cloned from A during testing, so
B is the same Debian; returning to A requires host `fastboot --set-active=a`). Backups on device:
`~/devinfo.bak`, `~/*.stock.bak`, `~/slotB-stock-bak/`.

# Online OTA design — REVISED (software-defined rootfs A/B, no fastboot)

The bootloader A/B slot cannot be switched from Debian, but it does not need to be: `root=` is
hardcoded to `super`, so the bootloader slot is irrelevant to rootfs updates. OTA is done at the
rootfs layer, exactly as the existing `OTA-Updates` repo does (initramfs picks the rootfs from a
userspace-controlled state file). That design is correct *because* of the slot constraint proven
above — not a workaround to be replaced.

- **Bootloader slot:** leave fixed (e.g. always A). Never attempt to switch it from userspace.
- **Rootfs A/B home:** an inactive rootfs target the initramfs can select — either real
  `rootfs_a`/`rootfs_b` partitions carved from the 229 GB `userdata`, or btrfs subvolumes in
  `userdata` (the `OTA-Updates` `rootpool` scheme). Needs the layout decision below.
- **Update flow (from Debian, over SSH):** write the new rootfs to the *inactive* rootfs target
  → flip the userspace state file → reboot. Initramfs mounts the new rootfs; validates and falls
  back to last-good on failure (software A/B rollback, no bootloader involvement).
- **Kernel / boot images:** `boot`/`vendor_boot` are slotted and bootloader-locked, so update
  them **in place on the current slot** (`dd` to `boot_<current>`). No A/B rollback for a bad
  kernel — do kernel/initramfs updates rarely and carefully; most breakage lives in the rootfs,
  which *is* protected. (A bad kernel needs host fastboot recovery.)
- **Fleet:** same felix layout everywhere → one tool + new rootfs image pushed over SSH/Ansible
  flashes every phone's inactive rootfs and flips its state file. No USB/fastboot per device.

`pixel-devinfo`'s role shrinks: it can *read/report* slot state (useful), and set
retry/successful/unbootable flags the bootloader *does* read for its own rollback — but it is
**not** the OTA slot-switch mechanism. The OTA mechanism is the initramfs + rootfs state file.

## Where the active slot actually lives — EXHAUSTIVELY TESTED (2026-06-18)

Question driving this: "Android switches slots from userspace (update_engine → boot_control
HAL), so it must be possible." We tested every userspace-writable store on felix:

| Store | Writable from Debian? | Controls slot? |
| --- | --- | --- |
| `devinfo` per-slot flags (0x30–0x37) | yes | **no** — bootloader rewrites to its own choice |
| `devinfo` selector region (0x6d–0x6f) | yes | **no** — bootloader rewrites it |
| full pristine `devinfo` image | yes | **no** — booted bootloader's choice anyway |
| GPT partition attributes | yes | **no** — empty/unused on boot_a/boot_b |
| `misc` / `bootloader_control` (AOSP std, magic `BCAB`, +CRC32) | yes | **no** — bootloader ignored a valid hand-built struct, didn't even read/clear it |
| `blenv` | yes | unused (zeros) |
| **RPMB / bootloader-private** | **no** (TEE-owned key) | **YES — authoritative** |

`fastboot --set-active=b` *does* switch (bootloader writes its TEE/RPMB-backed store, mirrors to
devinfo). We also wrote a valid AOSP `misc`/`bootloader_control` (=A) while that store said B →
it stayed B, i.e. `bootloader_control` **loses to the TEE store** once set (and fleet phones are
all fastboot-flashed, so it's always set). felix *does* use the standard `misc` A/B scheme
(`misc_virtual_ab_message` magic `0x56740AB0` with `source_slot` confirmed at misc `0x8000`).

**How Android/LineageOS switch slots (the key correction):** slot switching is **keyless** —
Google's keys are only for verified boot / signing images, NOT for `setActiveBootSlot`. That's
why unsigned LineageOS can switch slots. What custom ROMs reuse is **Google's vendor
`boot_control` HAL binary + the Trusty/TEE stack** (they keep the vendor partition). `bootctl
set-active-boot-slot` → that HAL → the TEE-backed store the bootloader reads.

**Conclusion for our bare Debian:** the slot lives in a TEE/RPMB-backed store reached via the
**vendor boot_control HAL over Trusty**, which Debian doesn't have. It's a *missing-HAL* problem,
**not** a keys problem and **not** "impossible" — it's standard Android functionality we lack the
userspace/TEE path for. Two ways to get it: (2) drive the `boot_control` Trusty TA from Debian
(if the felix kernel exposes the Trusty IPC driver and the TA is reachable), or (3) extract and
analyze Google's `boot_control` HAL from the felix vendor image to learn the exact path, then
port it. Both are real engineering; verify feasibility before committing. Otherwise use kexec.

### Trusty transport CONFIRMED PRESENT on Debian (2026-06-18) — option 2 is viable

The TEE pipe is already on the device under Debian:
- `/dev/trusty-ipc-dev0` (Trusty IPC transport) + `/dev/gsa0` (Google Security A-core) + `/dev/trusty-log0`.
- Loaded modules: `trusty_core`, `trusty_ipc`, `trusty_virtio`, `trusty_log`.
- dmesg: `gsa 17c90000.gsa-ns: TZ: com.android.trusty.gsa.hwmgr.tpu connected` — **secure world is live**.
- (`CONFIG_TEE` unset is irrelevant — that's generic OP-TEE; Google's Trusty driver is loaded.)

So the secure world (TEE) is running and the kernel transport exists — the **only** missing piece for
keyless, fastboot-free slot switching from Debian is the **userspace protocol**: the Trusty **port name**
the `boot_control` TA listens on and the **`setActiveBootSlot` message format**. On Tensor this likely
routes via the GSA service (`/dev/gsa0`). Get both by extracting Google's `android.hardware.boot` HAL (and
GSA lib) from the felix vendor image, then write a small Debian client over `/dev/trusty-ipc-dev0`
(libtrusty `tipc_connect` + write). This is now the most promising path to true bootloader-slot A/B
(kernel updates with real rollback) without Android or keys.

Next actions: (1) probe reachable Trusty ports via `/dev/trusty-ipc-dev0`; (2) pull the boot_control HAL +
GSA lib from the felix vendor image; (3) reverse port + request struct; (4) prototype the Debian client.

#### Probe result (2026-06-18) — boot control is NOT a reachable Trusty port; it's GSA-mediated

Built `pixel-bootctl` (flake-cross-compiled aarch64 static-musl; `status`/`probe`/`send` subcommands),
deployed to the device, and probed `/dev/trusty-ipc-dev0`:
- **Reachable** (CONNECTED): `keymaster`, `gatekeeper`, `storage.proxy`, `gsa.hwmgr.tpu`, `gsa.hwmgr.aoc`.
- **Not reachable** (ENOTCONN) under any guessed name: `boot_control`, `bootcontrol`, `boot`, `avb`, `hwbcc`,
  `rpmb`, `gsa.boot`, `gsa.bootctrl`, `gsa.slot`, `gsa.hwmgr.boot/storage/rpmb`, `bootloader`, `fastboot`, …

So `setActiveBootSlot` is **not** a guessable Trusty IPC service. On Tensor it is mediated by **GSA
firmware via `/dev/gsa0`** (Google's closed vendor HAL speaks a proprietary protocol to it). Guessing is
exhausted. To drive the real slot from Debian we must reverse Google's `boot_control`+GSA HAL from the
felix **vendor image** (factory zip) — the `/dev/gsa0` ioctl/message protocol — which is a real RE effort.
`pixel-bootctl` now has the transport plumbing (`probe`/`send`) ready for that work.

**Pragmatic recommendation stands: kexec-shim** (kernel A/B without a bootloader slot switch) is the
lower-risk path to the fleet goal unless/until the GSA protocol is reversed.

#### SOLVED (2026-06-18) — the slot switch is a UFS boot-LUN sysfs write. No fastboot/keys/GSA/Trusty.

The Trusty/GSA theory was WRONG. Reading the actual Tensor boot HAL source
(`device/google/gs-common/bootctrl/1.2/BootControl.cpp`), `setActiveBootSlot()`:
1. writes the **UFS boot-LUN attribute** via the Pixel kernel sysfs node
   **`/sys/devices/platform/<ufs>/pixel/boot_lun_enabled`** — `"1"`=slot A, `"2"`=slot B (this is
   the real switch: it selects which UFS boot LUN — `sdb`=A bootloaders, `sdc`=B — the chip boots);
2. updates the **devinfo** active/successful/retry flags as bookkeeping (DevInfo.h: 128-byte struct,
   magic `DEVI`, ab slot data at offset 48, same fields pixel-devinfo knew);
3. `markBootSuccessful` additionally calls `blowAR()` (anti-rollback bump) over Trusty — but the slot
   *switch* itself needs none of that.

**This is why every partition write failed and why LineageOS succeeds:** the lever is a plain
root-writable `/sys` knob exposed by the Pixel UFS kernel driver. LineageOS's boot HAL writes it; so
can bare Debian. We never knew the node existed.

**PROVEN on-device (felix, kalm@…138):** with `boot_lun_enabled` confirmed at `2` on slot B, we wrote
`1` (+ devinfo active=A) from Debian → rebooted → `slot_suffix=_a`. Then `pixel-bootctl set-active-slot
b` (the tool) → rebooted → `_b`. Full bidirectional slot switching from Debian userspace, no fastboot.

`pixel-bootctl set-active-slot <a|b>` now implements exactly this (auto-detects the `*.ufs/pixel/
boot_lun_enabled` node + writes devinfo flags). **Conclusion: bootloader-slot A/B OTA from Debian is
viable — kexec-shim is no longer required** (it remains a fallback). Kernel A/B OTA = flash inactive
boot/vendor_boot (+ rootfs) → `pixel-bootctl set-active-slot` → reboot → `confirm` post-boot.

## Kernel A/B OTA without fastboot — RECOMMENDED PATH: kexec shim

Since the bootloader slot is unreachable but rootfs A/B is software-controlled, decouple "the
kernel the bootloader loads" from "the kernel actually run":

1. Bootloader slot stays **fixed** (never switched).
2. `boot.img` becomes a small **stable shim** (kernel + initramfs) whose only job is to select
   the active **rootfs** slot (software A/B state file in `userdata`) and **`kexec` the real
   kernel from inside that rootfs**.
3. Real kernel + modules + initramfs live in the rootfs, A/B'd in software → kernel updates ride
   the rootfs update with the same software rollback (bad slot → fall back).

Gives full kernel + rootfs A/B OTA entirely from Debian over SSH, no fastboot. Requirements:
- Rebuild the felix kernel with **`CONFIG_KEXEC` / `CONFIG_KEXEC_FILE`** (currently **not set**;
  `junkyard-boot-img` controls the defconfig, so this is a one-line change).
- Add `kexec-tools` to the rootfs.
- Only the shim `boot.img` stays in-place/unprotected — tiny and rarely changed, acceptable.

## Net for the fleet

- Switch bootloader A/B slot from Debian: **No** (RPMB/TEE-locked; fastboot-only).
- Rootfs OTA without fastboot: **Yes** — software A/B (`OTA-Updates` model).
- Kernel OTA without fastboot, with rollback: **Yes** — via the kexec shim above.

## Rootfs / super — REVISED after on-device verification  ⚠️

The "flash rootfs to an inactive slot" idea is **not possible with the current partition
layout**, and this changes the plan:

- `super` (sda30, 7.9G ext4, label `rootfs`) **is the live, mounted root `/`** — a single
  partition with **no `_a`/`_b` slot**.
- `userdata` (sda31, 229G ext4) is plain data — **not** the btrfs `rootpool` with
  `rootfsA`/`rootfsB` subvolumes the `OTA-Updates` docs describe. This device is a plain
  `junkyard-boot-img` install, never converted to the OTA-Updates scheme.

So there is no inactive rootfs slot to write, and the only rootfs partition is the running
root (unsafe to overwrite online). Options (pending user decision):

1. **boot/vendor_boot only (deliverable now):** ship safe A/B online OTA for the slotted boot
   images today; leave rootfs updates out of scope for v1.
2. **Repartition for a real rootfs slot:** carve `userdata` into `rootfs_a`/`rootfs_b` (or add
   a `super_b`) so rootfs follows the devinfo slot like `boot`. Clean A/B, but needs a
   wipe/repartition of the 229G data partition and a build/flash-layout change.
3. **btrfs subvolumes in userdata (OTA-Updates scheme):** convert `userdata` to the `rootpool`
   btrfs layout; `online-flash` writes the inactive subvolume and boot is pointed at it.
   Reuses existing OTA machinery rather than a pure partition flip.

# Online OTA flow (`ota` subcommand)

1. Parse current `devinfo`; determine active slot `S` and inactive slot `S'`.
2. Resolve block devices via `/dev/disk/by-partlabel/` for `boot_S'`, `vendor_boot_S'`
   (and other slotted boot images as desired). Rootfs is included only under a layout that
   provides an inactive rootfs target (see Rootfs decision above) — otherwise it is skipped.
3. **Verify** each image: exists, fits target partition, sanity magic/hash; **refuse** if any
   resolved device belongs to active slot `S`.
4. `--dry-run` → print the resolved plan (devices, sizes, slot switch) and exit.
5. Write each image to its target device; `fsync`; verify.
6. Update `devinfo`: `S'.active=true`, `S.active=false`, `S'.retry_count=N`,
   `S'.successful=false`, `S'.unbootable=false`; write back to the devinfo block device.
7. `--reboot` → reboot. Bootloader boots `S'`; on failure it decrements retries and reverts
   to `S`.
8. After a healthy boot, `pixel-devinfo confirm` (systemd unit) marks `S'` successful.

# Milestones

1. **Refactor** `pixel-devinfo` into lib + subcommand skeleton; move devinfo parse/serialize
   into a reusable module; add block-device I/O alongside file I/O. (No behavior change to
   existing devinfo editing.)
2. **`flash`** subcommand: slot resolution, `by-partlabel` device mapping, size/magic
   verification, active-slot refusal, `--dry-run`, streamed write + verify. Targets the
   slotted boot images first (`boot`, `vendor_boot`) — deliverable on the current layout.
3. **Decide rootfs slot model** (option 1/2/3 above) — only option 1 (boot-only) needs no
   layout change; options 2/3 require repartition or btrfs conversion before rootfs OTA works.
4. **`ota`** subcommand: orchestrate trio flash + devinfo switch with rollback flags.
5. **`confirm`** subcommand + systemd unit to mark the slot successful post-boot.
6. **Wire into the image build** (`junkyard-boot-img` overlay / `OTA-Updates`): ship the
   binary + units in the rootfs; document the new online-OTA path and retire the dracut
   boot-time update phase.
7. **Docs**: update `pixel-devinfo` README; cross-link from `OTA-Updates` and
   `junkyard-boot-img`.

# Open questions / risks

- **Felix partition table** — ✅ verified: `boot`/`vendor_boot`/etc. are real A/B slots;
  rootfs (`super`/sda30) is a single non-slotted partition = the live root; `userdata`
  (sda31) is plain ext4, not btrfs rootpool. Open: which rootfs option (1/2/3) to pursue.
- **devinfo device path & format** — ✅ verified: `/dev/disk/by-partlabel/devinfo` (sdd1,
  8192B, `DEVI` magic, v`03 00 0f 00`); also holds `DIUS`/`DIFR` factory strings to preserve.
  Still TODO: confirm `pixel-devinfo`'s field offsets decode the active/retry/successful bits
  correctly against this exact version (read-only diff before any write).
- **Writing a mounted rootfs** — under options 2/3 the inactive rootfs target must be unmounted
  while writing; confirm nothing holds it open. (N/A for boot-only v1.)
- **Privileges** — runs as root; document required capabilities and the systemd unit context.
- **Power-loss safety** — `fsync` + verify after each write; the active slot is never touched,
  so an interrupted OTA leaves the running system intact.
- **Retiring `OTA-Updates`** — coordinate removal of the dracut update/select/patch phases once
  online OTA is proven, to avoid two mechanisms fighting over slot/rootfs state.
