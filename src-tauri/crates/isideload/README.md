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

- [idevice](https://github.com/jkcoxson/idevice) by [jkcoxson](https://github.com/jkcoxson) crate is used to communicate with the device
- [apple-codesign-quick](https://github.com/Dadoum/apple-codesign-quick) by [Dadoum](https://github.com/Dadoum) for codesigning and entitlements
- [Impactor](https://github.com/claration/Impactor) by [claration](https://github.com/claration) was used as a reference for cryptography operations (converting certs to p12, etc.).
- [Sideloader](https://github.com/Dadoum/Sideloader) by [Dadoum](https://github.com/Dadoum) was used as a reference for how apple private developer endpoints work
