# rr_reader

Rust firmware skeleton for the RR reader hardware. It is initialized from the
same hardware assumptions as `crosspoint-reader`:

- MCU/board: ESP32-C3, PlatformIO board equivalent `esp32-c3-devkitm-1`
- Flash: 16MB, DIO mode
- Partition layout: copied from CrossPoint, with two OTA app slots and coredump
- Runtime: ESP-IDF Rust `std` stack

## Layout

- `src/main.rs`: firmware entry point and startup order
- `src/hardware.rs`: board/model constants and hardware boundary
- `src/display.rs`: display boundary placeholder
- `src/power.rs`: wakeup/power handling boundary
- `src/app.rs`: main reader application loop
- `partitions.csv`: CrossPoint-compatible 16MB partition table
- `sdkconfig.defaults`: ESP-IDF defaults for ESP32-C3 and the custom partition table

## Tooling

Install the ESP Rust tools if they are not already available:

```sh
cargo install ldproxy espflash
```

On Arch Linux, Espressif's `esp-clang` package for ESP-IDF 5.3 expects
`libxml2.so.2`. If `cargo build` fails while installing ESP-IDF tools with a
`libxml2.so.2` loader error, install an Arch-compatible legacy libxml2 package
before building again.

Build:

```sh
cargo build
```

Flash and monitor:

```sh
cargo run
```

## Next Hardware Work

The current project is intentionally a clean Rust skeleton. The CrossPoint C++
project uses `open-x4-sdk` for GPIO, power, SD card, and E-Ink display support.
Those drivers need Rust equivalents or FFI bindings before the placeholders in
`hardware.rs` and `display.rs` can operate real hardware.
