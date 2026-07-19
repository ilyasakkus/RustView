# RustView distributions

RustView release archives are generated in this directory by the platform
packaging scripts and the GitHub Actions release workflow. Generated archives are
ignored by Git because downloadable binaries belong in GitHub Releases rather
than in the source history.

Each tagged release publishes these files:

- `RustView-macOS-universal.zip`
- `RustView-Windows-x86_64.zip`
- `RustView-Linux-x86_64.tar.gz`
- `SHA256SUMS.txt`

The archives contain the RustView desktop application, the optional blind relay
server, the project README, and the MIT license. See the repository
[README](../README.md#download) for download and launch instructions.
