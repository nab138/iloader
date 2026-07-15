# isideload

[![Build isideload](https://github.com/nab138/isideload/actions/workflows/build.yml/badge.svg)](https://github.com/nab138/isideload/actions/workflows/build.yml)

A Rust library for sideloading iOS applications using an Apple ID. Used in [CrossCode](https://github.com/nab138/CrossCode) and [iloader](https://iloader.app).

## Usage

**You must call `isideload::init()` at the start of your program to ensure that errors are properly reported.** If you don't, errors related to network requests will not show any details.

A full example is available is in [examples/minimal](examples/minimal/).

## TODO

Things left todo before the rewrite is considered finished

- Proper entitlement handling
  - actually parse macho files and stuff, right now it just uses the bare minimum and applies extra entitlements for livecontainer
- Reduce duplicate dependencies
  - partially just need to wait for the rust crypto ecosystem to get through another release cycle
- More parallelism and caching for better performance

## Licensing

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

## Credits

- The [idevice](https://github.com/jkcoxson/idevice) crate is used to communicate with the device
- A [modified version of apple-platform-rs](https://github.com/nab138/isideload-apple-platform-rs) was used for codesigning, based off [plume-apple-platform-rs](https://github.com/plumeimpactor/plume-apple-platform-rs)
- [Impactor](https://github.com/khcrysalis/Impactor) was used as a reference for cryptography, codesigning, and provision file parsing.
- [Sideloader](https://github.com/Dadoum/Sideloader) was used as a reference for how apple private developer endpoints work
