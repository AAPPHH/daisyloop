# Security and safety

daisyloop writes HID reports to a pump and fan controller (Aqua Computer D5 NEXT). Wrong values, a wrong model or a bug can slow or stop the pump or fans and stop cooling, which can damage hardware.

## Safeguards

- Writes are off by default: `writes_enabled = false`. Without it the daemon only reads.
- The configuration that enables writes and sets limits lives in `%ProgramFiles%\daisyloop\config.toml`, which is writable by administrators only. The user config cannot override security keys.
- The pump duty has a floor of 15 %.
- Above `water_limit` (default 48 C) the daemon hands control back to the device firmware (firmware takeover).
- The daemon releases its channels to the firmware on a clean stop, and a watchdog task restarts it.

## Use at your own risk

The software is provided without warranty (see `LICENSE`). The models were fitted on one specific loop and must be retrained for other hardware (see `README.md` and `docs/steptest.md`). Monitor temperatures yourself when you enable writes for the first time and keep the firmware fallback available.

## Reporting issues

Report security or safety problems by opening an issue at https://github.com/AAPPHH/daisyloop/issues (for anything that could make the software unsafe, use GitHub private vulnerability reporting under the Security tab). Do not include serial numbers or personal data. For problems that could damage hardware, describe the configuration and the log lines, and say clearly that it is safety-relevant.
