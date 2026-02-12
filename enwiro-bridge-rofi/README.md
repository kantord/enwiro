# Enwiro rofi bridge

Browse and activate enwiro environments from [rofi](https://github.com/davatorium/rofi).

## Installation

```
cargo install enwiro-bridge-rofi
```

## Usage

Run rofi with the bridge as a script mode:

```
rofi -show enwiro -modi "enwiro:enwiro-bridge-rofi"
```

This will list all available environments and recipes. Selecting an entry
runs `enwiro activate` to switch to (or create) the corresponding workspace.
