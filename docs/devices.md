# Devices

What is in the machine, who drives each device, and what a driver may do
with its own. The kernel's side is in `../quark/docs/abi.md` (*Devices
(0xA8)*, and `PciDevice` under *Capabilities*); this is the side that runs
on it.

## The device manager

The kernel finds every PCI function once, at boot, sizes its BARs, and keeps
what it found. A program reaches a device only with the capability for it
(`CAP_TYPE_PCI_DEVICE`): its configuration, its BARs, its claim and its
interrupt by message all go with that one capability, and the ports devices
were once configured through are the kernel's. `init` is started holding
every device; it gives that to `devmgr`, the device manager, and to nobody
else.

The device manager is the one program that starts a driver for a device. It
lists what the kernel found, and for each device it finds a driver for, it
starts the driver once, holding:

| What | Why |
|---|---|
| `PciDevice` for that device, and no other | everything about the device: see below |
| `Irq` for the device's interrupt line, where it has one | the firmware's wiring, from the device's configuration; an IDE controller in compatibility mode has 14 and 15 |
| what the driver's manifest asks for | as any spawner grants: what the device manager holds — a driver's band, frames, an interrupt line — and nothing else |
| its standard output | the console, as the device manager's own |
| `argv[1]` = the device's address, `BB:DD.F` | which device is its own (`quark_rt::pci::this_device`) |

It is the drivers' parent and watches them. A driver that ends is collected,
and its device has no driver until the machine starts again: nothing starts
a driver a second time yet.

## A driver says what it drives

In its manifest, beside what it asks for:

```rust
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x10EC, 0x8139),          // a vendor's device
    // CapReq::drives_class(0x01, 0x01),     // any IDE controller
    // CapReq::drives_interface(0x01, 0x08, 0x02),  // an NVMe controller
    CapReq::phys_alloc(64),
]);
```

A match is `key & mask == value` over `vendor << 48 | device << 32 | class
<< 24 | subclass << 16 | interface << 8`, and a manifest may hold several.
They are not capabilities: a spawner skips them (`manifest::grant_image`),
so a driver started some other way holds no device, and says so.

## Where drivers come from

- **The boot image**, for a device the root is on: its driver has to be
  running before there is a filesystem to read one from. `init` reads each
  program in the boot image, and one whose manifest says it drives something
  is offered to the device manager rather than started: lent with the call
  (`TAG_OFFER`). The disks' drivers are, `DISK`, `AHCI`, `NVME` and
  `VIRTBLK`, since a root can be on any of them; and the network cards',
  `RTL8139` and `VIRTNET`, so that the network is up before anybody is
  asked to log in.
- **`/usr/lib/drivers`**, for everything else. When `init` has a root it
  tells the device manager so (`TAG_FILES`), before anything in
  `/etc/init.conf` runs, and the device manager reads every file there. A
  distribution installs a driver there; `edu` is one. The device manager
  answers only once every driver it started — the boot image's too — has
  answered a call, two seconds at most each: what a driver says as it
  starts is said before the session's first prompt. A USB controller's
  driver answers once what was plugged in as the machine started has been
  seen to.

A device already driven is not offered again, so a driver in both places is
started from the first. The device manager takes either request from its
parent and from nobody else: a program that could hand it a driver would be
handed a device.

## What a driver does with its device

Everything through `quark_rt::pci`, by the address it was given:

- `pci::info(at)` — what the kernel found: ids, class, interrupt line and
  pin, MSI and MSI-X, the BARs with their lengths.
- `pci::ports(at, n, slot)` and `pci::map_bar(at, n, virt, slot)` — mint the
  ports of an I/O BAR, or a range over a memory BAR's pages, and map the
  latter. Nothing outside the device's BARs can be minted, and a BAR whose
  pages another device's BAR shares cannot be mapped at all.
- `pci::claim(at)` — the device is this program's, and on a machine with an
  IOMMU reaches the frames this program asked for (`sys_phys_alloc`) and
  nothing else. Before it copies any memory: `pci::enable(at,
  COMMAND_MASTER)` is refused until then.
- `pci::interrupt(at, map)` — the device's interrupt, the best it has: a
  message the kernel aims it at, where it has MSI (`pci::message` alone);
  else entry 0 of its MSI-X table, which `map` maps from the BAR it is in,
  with a message from the kernel written into it; else the line the device
  manager gave, registered. Never MSI-X on a device that has MSI: the
  kernel turns MSI on as it aims it, and a device with both on does what it
  likes. A line is acknowledged after each interrupt (`sys_irq_ack`); a
  message is not.
- `pci::read32`, `write16` and the rest — the device's configuration. A BAR
  and the MSI capability are the kernel's, and a write to one is refused
  (`Refused::NotAllowed`): where a device is, and where its message goes,
  are not its driver's to change.

## Network cards

A card's driver serves `quark_rt::nic` to the stack (`net`) and registers
as the first of `eth0` to `eth7` nobody has. `rtl8139` is the oldest card
QEMU has, and `virtnet` virtio's (below). `e1000` is Intel's gigabit
family — the 82540EM a PC in QEMU is given, the 82545EM, and the 82574L,
which is `e1000e` and the q35 machine's — through its first BAR's
registers: a ring of thirty-two descriptors that frames come into and
eight they go out of, each naming its buffer by a sixty-four-bit address,
in the oldest form every card of the family has, with nothing offloaded.
Its address is where the card put it, in its first receive address, or
else its EEPROM's. Its interrupt is a message where it has MSI (the
82574L) and its line where it has not. Not done: the newer cards of the
family (the I217 to I219 a laptop has, which want their PHY seen to),
offloads, and more than one queue.

## virtio

A virtio device (`quark_rt::virtio`) is driven through the same capability:
its structures — common configuration, where a queue is told it has work,
the interrupt's status, its own configuration — are named by capabilities of
the vendor's kind in its configuration space, each a BAR and an offset, and
mapped from its BARs. A queue is a page of the driver's own memory, so on a
machine with an IOMMU it is among what the device reaches. Its interrupt is
one MSI-X message (`pci::interrupt`), which each queue is told to send — or,
with no MSI-X, its line. Only the modern half of a transitional device is
driven.

A virtio GPU (`virtgpu`) shows a picture the host keeps, copied from memory
the guest gives it when the guest says. Its driver is given the screen's
memory by the kernel (`SYS_DISPLAY_MEMORY`: nobody's, kept for the device,
reached by it), makes the picture from it and shows it, and offers the
display to `fb`, which lends it out as it does the bootloader's framebuffer;
what whoever has the display draws, it says (`quark_rt::display`), and the
driver copies that.

## USB

An xHCI controller's driver, `usb`, is in the boot image and drives what is
plugged into the controller as well as the controller: one program for all
of it, whose first thread alone touches the controller.

- **What is plugged in** is given an address, asked what it is, and
  configured: its first configuration, and each interface of it that is a
  hub, a keyboard or a mouse that speaks the boot protocol (what a BIOS
  reads), or a disk that speaks bulk-only SCSI. Plugged in or pulled out at
  any time, at a root port or a hub's.
- **Keys and movement** go to `input`. The driver offers itself
  (`TAG_INPUT_SOURCE`), and `input` asks the device manager whether it is a
  driver it started (`TAG_IS_DRIVER`) before it takes any: a program that
  could make itself a source of keys could type into the console. A key is
  said the way the i8042's driver says one (`quark_rt::keys`), and a key held
  down is typed again, after half a second and thirty times a second.
- **A disk** is a thread of the program serving `block` as the *last* free
  `diskN` — `disk3` downwards, so that the disks the machine started with
  keep the first names — its reads and writes handed to the first thread.
  Pulled out, the thread ends and its name goes with it; put back, it is a
  disk again, by the same name if nothing took it meanwhile.
- **What there is** is answered by a second thread, registered as the first
  free of `usb0` to `usb3` (`quark_rt::usb`); `lsusb` asks every one.

Not driven yet: USB 3 hubs (a USB 3 device on a root port is), a keyboard or
mouse that does not speak the boot protocol — a tablet's absolute pointer,
which needs its report descriptor read — more than one unit of a disk, a disk
whose blocks are not 512 bytes, and anything isochronous (sound, cameras).

## Asking what is there

The device manager registers as `devices` and answers anybody:

| Tag | Asks | Answer |
|---|---|---|
| 3 | the device at index `data[0]`, in order of address | the first three words of the kernel's description (address, header, pin and line, MSI and MSI-X; ids; class and revision), the driver's task or 0, and its name in two words |
| 4 | BAR `data[1]` of the device at address `data[0]` | where, how long, flags (1 ports, 2 64-bit, 4 prefetchable) |
| 5 | is the program `data[0]` (a space id) a driver started here? | `[1 if it is, else 0]` |

Past the last device, or a BAR there is not, the answer's tag is
`u64::MAX`. `quark_rt::devices` is the client: `lspci` (`-v` for
interrupts, BARs and drivers) asks tags 3 and 4, and `input` tag 5. Tags 1
and 2 are `init`'s, above.
