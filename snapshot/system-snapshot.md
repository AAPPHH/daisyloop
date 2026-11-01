# daisyloop system snapshot (2026-11-01, read-only)

Raw outputs: `C:\clones\daisyloop\snapshot\raw\*.txt`. Everything collected without writes to hardware, except: liquidctl `connect()` on the two Lighting Node CORE (CommanderPro driver) issues its normal firmware/handshake query over HID before `get_status()`; no lighting/fan/pump set commands were sent. No services stopped, no usbipd attach/bind.

## Kernel
- Host: Windows 11 Pro 10.0.26200 (build 26200, `ver` 10.0.26200.9711), x64. No Linux kernel on the host; hwmon does not exist on Windows.
- Board: MSI MPG Z890 EDGE TI WIFI (MS-7E19), BIOS ALASKA 1072009. CPU: Intel Core Ultra 9 285K (24 cores / 24 threads, 3.7 GHz max). iGPU: Intel Graphics 32.0.101.5869.
- WSL: distro Ubuntu 24.04.1 (WSL2, stopped before the snapshot, started for it), also docker-desktop (stopped). WSL kernel `Linux 6.6.87.2-microsoft-standard-WSL2` (the /lib/modules dir also has 5.15.167.4-microsoft-standard-WSL2). See `raw/wsl.txt`.
- WSL kernel config: `CONFIG_HWMON=y`, `# CONFIG_SENSORS_AQUACOMPUTER_D5NEXT is not set`, `CONFIG_HIDRAW=y`, `CONFIG_USB_HID=m`, `CONFIG_USB_HIDDEV=y`, `CONFIG_USBIP_CORE=m`, `CONFIG_USBIP_VHCI_HCD=m` (8 ports, 1 hcd), `CONFIG_USBIP_HOST=m`.

## USB devices
Windows `Get-PnpDevice -PresentOnly` (full list `raw/pnp_usb_hid.txt`, props `raw/pnp_props.txt`); `lsusb` equivalent for the relevant devices:

| VID:PID | Device | Windows driver | Parent / location |
|---|---|---|---|
| 0C70:F00E | Aqua Computer D5 NEXT, serial 00000-00000, composite: MI_00 "D5 NEXT" (WinUSB), MI_01 HID vendor-defined (HidUsb) | MI_00 winusb.inf, MI_01 input.inf | location 0080.0014.0000.009.002 |
| 1B1C:0C1A | CORSAIR Lighting Node CORE, serial 00000000000000000000000000000001 | HidUsb | hub VID_1A40&PID_0201 (generic hub), Port_#0004.Hub_#0011 |
| 1B1C:0C1A | CORSAIR Lighting Node CORE, serial 00000000000000000000000000000002 | HidUsb | same hub, Port_#0002.Hub_#0011 |

- No 1B1C:0C3F (iCUE LINK System Hub) present. No other VID_1B1C or VID_0C70 device.
- Other USB: Razer (1532:02A6, 00A4, 0C05), Logitech 046D:0944 headset, 0DB0:0076 MSI, 05E3 hubs (Genesys), 2109 hubs (VIA), 03F0 HP hub, 1A40 hub. Two "Unbekanntes USB-Gerät" entries (VID_0000&PID_0002, error requesting device descriptor).
- usbipd: not installed (`winget list usbipd` empty, `C:\Program Files\usbipd-win` absent). `usbipd list` unavailable.

## HID devices
Python hidapi 0.15.0 in isolated venv (scratchpad `venv`), 46 HID collections total, full list in `raw/hid_enum.txt`. Target devices:

| VID:PID | Manufacturer / product | Serial | usage_page | usage | interface | release | Path |
|---|---|---|---|---|---|---|---|
| 0C70:F00E | Aqua Computer GmbH & Co. KG / "Device" | 00000-00000 | 0xFF00 | 0x0001 | 1 | 0x0100 | `\\?\HID#VID_0C70&PID_F00E&MI_01#8&00000001&0&0000#{4d1e55b2-...}` |
| 1B1C:0C1A | Corsair / CORSAIR Lighting Node CORE | 00000000000000000000000000000002 | 0xFFC0 | 0x0001 | 0 | 0x0009 | `...HID#VID_1B1C&PID_0C1A#8&00000002&0&0000...` |
| 1B1C:0C1A | Corsair / CORSAIR Lighting Node CORE | 00000000000000000000000000000001 | 0xFFC0 | 0x0001 | 0 | 0x0009 | `...HID#VID_1B1C&PID_0C1A#8&00000003&0&0000...` |

- All three were enumerated and their strings read without exclusive-open errors; hidapi sees them while aquasuite is running (Windows HID allows shared opens).
- Other HID vendors present: 1532 (Razer, 34 collections), 046D (Logitech, 7), 0DB0, 05E3.

## hwmon
- Windows: no hwmon. `Get-CimInstance MSAcpi_ThermalZoneTemperature -Namespace root/wmi` -> "Zugriff verweigert" (needs admin; `raw/thermal.txt`).
- WSL Ubuntu: `/sys/class/hwmon/` is empty (no hwmon devices at all, no coretemp/k10temp either), no `/sys/bus/usb`, no `/dev/hidraw*`, no `/sys/class/hidraw`, `lsusb` not installed.
- D5 Next values available only through liquidctl/HID on Windows (see liquidctl section), live reading at snapshot time: liquid 33.21 C, pump 4818 rpm / 18.19 W / 12.08 V / 1.506 A, fan port 1455 rpm / 9.3 W / 12.08 V / 0.77 A, +5V 5.02 V, +12V 12.08 V.

## aquacomputer_d5next driver
- WSL: `lsmod | grep aqua` empty; `modinfo aquacomputer_d5next` -> "Module not found"; `CONFIG_SENSORS_AQUACOMPUTER_D5NEXT` is not set in `/proc/config.gz`; `modules.builtin`/`modules.dep` contain no match. Driver is not available in the stock WSL kernel (6.6.87.2).
- Driver-side prerequisites present in WSL kernel: HIDRAW=y, USB_HID=m, USBIP_VHCI_HCD=m (would allow usbip attach of the D5 into WSL, but a custom kernel with the D5NEXT option is still required). usbipd-win is not installed on host.
- Upstream driver reference (mainline since 6.0 as `aquacomputer_d5next`) exposes temp/fan/pwm/power/voltage/current channels via sysfs hwmon; not verifiable here. liquidctl's `aquacomputer.py` at `C:\clones\daisyloop\refs\liquidctl\liquidctl\driver\aquacomputer.py` implements the same HID report for D5 Next.

## liquidctl
- Not installed system-wide (`which liquidctl` not found on Windows; not found in WSL either). Installed only in scratchpad venv: liquidctl 1.16.0 + hidapi 0.15.0.
- `liquidctl list --verbose` and `liquidctl status` both crash identically before output: `usb.core.USBError: [Errno None] Other error` in `libusb_get_device_list` (pyusb/libusb backend fails to enumerate; raw: `raw/liquidctl_list.txt`, `raw/liquidctl_status.txt`). Cause not determined (no libusb-compatible backend/driver binding for the bus; PyUsb bus is the one failing).
- Workaround run with the PyUsb bus stubbed (HID bus only), `raw/liquidctl_hid_only.txt`:
  - Aquacomputer D5 Next (0C70:F00E, driver `Aquacomputer`): status read OK while aquasuite service runs: Liquid temperature 33.21 C, Pump speed 4818 rpm, Pump power 18.19 W, Pump voltage 12.08 V, Pump current 1.506 A, Fan speed 1455 rpm, Fan power 9.3 W, Fan voltage 12.08 V, Fan current 0.77 A, +5V 5.02 V, +12V 12.08 V.
  - Corsair Lighting Node Core x2 (1B1C:0C1A, driver `CommanderPro`): detected, status empty list (no sensors; RGB-only).
- liquidctl driver files for these devices: `aquacomputer.py` (D5 Next), `commander_pro.py` (Lighting Node Core), `commander_core.py` (not matching the hardware here).

## udev rules
- Windows: n/a.
- WSL: `ls /etc/udev/rules.d /lib/udev/rules.d | grep -iE 'aqua|corsair|liquidctl|openlinkhub|hid'` returned nothing. No liquidctl/OpenLinkHub rules installed in WSL.

## Services
| Software | Installed | Running | Path / note |
|---|---|---|---|
| aquasuite | yes ("aquasuite" X) | yes: service `Aqua Computer Service` (Automatic, Running) PID 5460, `AquaComputerServiceHelper.exe` PID 22796, `aquasuite.exe` PID 48848 | `C:\Program Files\aquasuite\` |
| Corsair iCUE / Corsair services | no (no matching service, process, uninstall entry, or `C:\Program Files\Corsair*`) | no | Lighting Nodes run on generic HidUsb |
| CoolerControl | no | no | WSL: `coolercontrold` not found |
| OpenLinkHub | not installed (only source clone) | no | `C:\clones\daisyloop\refs\OpenLinkHub` |
| FanControl, SignalRGB, OpenRGB, LibreHardwareMonitor | no | no | |
| HWiNFO64 8.24 | yes | not running at snapshot | `C:\Program Files\HWiNFO64\` |
| liquidctl | venv only | n/a | |
- Device occupancy: aquasuite holds the D5 NEXT (MI_00 on WinUSB, MI_01 HID). The HID interface is still readable concurrently through hidapi/liquidctl (shared access), so reading is possible; concurrent writes would compete with aquasuite. Lighting Nodes are not held by any Corsair software.

## GPU/CPU sensors
- GPU NVIDIA GeForce RTX 4080, driver 610.88 (Windows 32.0.16.1088): temperature.gpu 35 C (nvidia-smi -q at the same time: 37 C), power.draw 51.23 W (instantaneous 53.99 W, average 63.71 W), fan.speed 0 %, graphics clock 2430 MHz. Raw: `raw/gpu_csv.txt`, `raw/gpu_q.txt`.
- CPU: Core Ultra 9 285K, no readable temp/power via standard Windows interfaces here (ACPI thermal zone requires admin; no LibreHardwareMonitor present). HWiNFO64 is installed and could provide it (shared memory or log) but is not running. In WSL there is no coretemp/k10temp hwmon (WSL2 exposes no host sensors).
- Intel Arrow Lake uses `coretemp` on native Linux; not applicable inside WSL.

## Conclusions (facts only)
- Attached target hardware: D5 NEXT 0C70:F00E (HID usage page 0xFF00/0x0001, if 1) and two Lighting Node CORE 1B1C:0C1A (usage page 0xFFC0/0x0001, if 0). No iCUE LINK System Hub 1B1C:0C3F is attached; the Link-hub path cannot be tested on this machine.
- The D5 Next is readable via HID on Windows with aquasuite running (liquidctl HID bus, shared read). Write access via HID/WinUSB was not tested (read-only constraint); aquasuite owns the device.
- hwmon/sysfs access to the D5 is not available now: WSL kernel 6.6.87.2 lacks `CONFIG_SENSORS_AQUACOMPUTER_D5NEXT`, WSL has no USB passthrough bridge (usbipd-win not installed, no hidraw nodes, no /sys/bus/usb), `/sys/class/hwmon` empty.
- Requirements for the planned hwmon path: install usbipd-win on host, build custom WSL kernel with `CONFIG_SENSORS_AQUACOMPUTER_D5NEXT=m` (HIDRAW and USBIP_VHCI_HCD already present), stop/release aquasuite from the D5 before attach (attaching moves the device away from Windows).
- liquidctl's default CLI fails on this host with a libusb enumeration error; HID-only enumeration works.
- No udev rules for aqua/corsair/liquidctl/openlinkhub in WSL; no iCUE/CoolerControl/OpenLinkHub/FanControl on host.
- Reference clones exist: `C:\clones\daisyloop\refs\liquidctl` (aquacomputer.py, commander_pro.py, commander_core.py) and `C:\clones\daisyloop\refs\OpenLinkHub` (src/devices/*).
