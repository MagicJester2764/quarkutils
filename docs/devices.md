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
    // CapReq::drives_interface(0x01, 0x08, 0x02),  // NVMe
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
  (`TAG_OFFER`). The disks' drivers are, `DISK` and `VIRTBLK`, since a
  root can be on either; and the network cards', `RTL8139` and `VIRTNET`,
  so that the network is up before anybody is asked to log in.
- **`/usr/lib/drivers`**, for everything else. When `init` has a root it
  tells the device manager so (`TAG_FILES`), before anything in
  `/etc/init.conf` runs, and the device manager reads every file there. A
  distribution installs a driver there; `edu` is one. The device manager
  answers only once each driver it started there has reached its loop (a
  call to it has been taken), two seconds at most each: what a driver says
  as it starts is said before the session's first prompt.

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
- `pci::message(at)` — an interrupt of the device's own, which the kernel
  aims the device at. Or the line the device manager gave, with
  `sys_irq_register`.
- `pci::read32`, `write16` and the rest — the device's configuration. A BAR
  and the MSI capability are the kernel's, and a write to one is refused
  (`Refused::NotAllowed`): where a device is, and where its message goes,
  are not its driver's to change.

## virtio

A virtio device (`quark_rt::virtio`) is driven through the same capability:
its structures — common configuration, where a queue is told it has work,
the interrupt's status, its own configuration — are named by capabilities of
the vendor's kind in its configuration space, each a BAR and an offset, and
mapped from its BARs. A queue is a page of the driver's own memory, so on a
machine with an IOMMU it is among what the device reaches. Its interrupt is
one MSI-X message: the kernel allocates it (`SYS_MSI_ALLOC` for the device)
and the driver writes it into entry 0 of the device's table, which is in
one of its BARs — or, with no MSI-X, its line. Only the modern half of a
transitional device is driven.

## Asking what is there

The device manager registers as `devices` and answers anybody:

| Tag | Asks | Answer |
|---|---|---|
| 3 | the device at index `data[0]`, in order of address | the first three words of the kernel's description (address, header, pin and line, MSI and MSI-X; ids; class and revision), the driver's task or 0, and its name in two words |
| 4 | BAR `data[1]` of the device at address `data[0]` | where, how long, flags (1 ports, 2 64-bit, 4 prefetchable) |

Past the last device, or a BAR there is not, the answer's tag is
`u64::MAX`. `quark_rt::devices` is the client, and `lspci` (`-v` for
interrupts, BARs and drivers) is all it is used for so far. Tags 1 and 2 are
`init`'s, above.
