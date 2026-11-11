# AirLink

Low-latency, high-fidelity two-way P2P audio link. Both ends run the same binary.

## Codec choice

| Mode | What it is | Codec delay | Bandwidth (stereo) | Use it for |
|---|---|---|---|---|
| `--codec opus` (default) | Opus, CELT-only restricted low-delay mode, CBR | 2.5 ms lookahead | 256 kb/s (up to 510) | Internet links, perceptually transparent at 256k+ |
| `--codec pcm` | Linear 24-bit / 48 kHz (L24) | 0 ms | 2.3 Mb/s | LAN or fat, clean links, bit-perfect audio |

The default is 5 ms frames, a 20 ms jitter buffer and 1× redundancy, which gives about 27.5 ms
one-way plus network and soundcard buffers. On a LAN, `--frame 2.5 --jitter 5` gets you under
10 ms.

## Install

Download from the GitHub Releases page:

| Platform | File | Notes |
|---|---|---|
| Windows 10/11 x64 | `AirLink-<ver>-windows-x64-setup.exe` | Setup wizard with Start Menu shortcut and uninstaller. Optionally adds a firewall rule so you can answer calls. |
| macOS 11+ (Apple Silicon + Intel) | `AirLink-<ver>-macos-universal.dmg` | Drag AirLink to Applications. Asks for microphone access on first call. |
| Debian/Ubuntu/Pi OS (amd64, arm64) | `airlink_<ver>-1_<arch>.deb` | `sudo apt install ./airlink_*.deb`. Adds a menu entry. `airlink --headless` for servers. |

**Unsigned builds:** until the app is code-signed, macOS will say it "can't be checked". Right-click
AirLink.app and choose **Open** once, or run `xattr -dr com.apple.quarantine /Applications/AirLink.app`.
On Windows, SmartScreen will ask you to confirm: click **More info → Run anyway**.

## Building installers

Push a tag to have GitHub Actions build every installer and publish a Release:

```sh
git tag v0.3.0 && git push origin v0.3.0
```

Or build locally:

```sh
# macOS -> dist/AirLink-<ver>-macos-universal.dmg
brew install cmake
./packaging/macos/build.sh
# Apple Silicon only (faster):  ARCHS=aarch64 ./packaging/macos/build.sh
# Signed + notarised: set MACOS_SIGN_IDENTITY, APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD
```

```powershell
# Windows -> dist\AirLink-<ver>-windows-x64-setup.exe
winget install Kitware.CMake JRSoftware.InnoSetup
.\packaging\windows\build.ps1
```

```sh
# Linux -> target/debian/airlink_<ver>-1_<arch>.deb
sudo apt install libasound2-dev libopus-dev pkg-config cmake \
  libxkbcommon-dev libgl1-mesa-dev libwayland-dev libx11-dev
cargo install cargo-deb
cargo deb
```

The macOS and Windows builds compile libopus from source and link it statically, so they
have no external dependencies. The .deb depends on the system `libopus0` and `libasound2`.

## Build from source

```sh
cargo build --release && ./target/release/airlink
```
(Install the same dependencies as above first.)

## GUI

- **Call / Answer**: Call needs the peer address. Answer waits for whoever calls (forward UDP 5004).
- Codec, bitrate, frame size, channels, jitter buffer, redundancy, input/output device
- Live send/receive peak meters with an −18 dBFS alignment mark
- Status: Idle / Waiting / Calling / Connected (amber when lossy)
- Stats: latency estimate, buffer depth, packets/s, concealed, late, underruns, drift fixes

## Headless

```sh
airlink --list
airlink --headless -i "Scarlett" -o "Scarlett"          # studio end, answers
airlink --peer studio.example.com -i "MacBook"           # remote end, calls
```

### Useful presets

```sh
# LAN, bit-perfect, lowest latency
airlink --peer 192.168.1.50 --codec pcm --frame 2.5 --jitter 5

# Internet, transparent stereo
airlink --peer studio.example.com --bitrate 256 --frame 5 --jitter 20 --redundancy 1

# Lossy 4G, tougher
airlink --peer studio.example.com --bitrate 192 --frame 10 --jitter 60 --redundancy 2
```

## How it works

- **Capture**: the soundcard callback slices audio into frames (2.5/5/10/20 ms) and hands
  them to the encoder thread without blocking.
- **Transport**: one UDP packet per frame, with a 16-byte header and a 64-bit sequence
  number. With `--redundancy N`, each packet also carries the previous N frames, so a single
  lost packet is recovered with no extra delay. (In CELT-only mode, Opus has no in-band FEC,
  so this does that job instead.)
- **Jitter buffer**: reorders packets by sequence number and pre-buffers to the target depth.
  Gaps are concealed with Opus PLC. Clock drift between the two soundcards is corrected by
  dropping or inserting one frame when the buffer stays outside its band for a while.
- **Stats line**: shows packets/s, concealed frames, late packets, buffer depth, underruns and
  drift corrections.

## Roadmap

- Adaptive jitter target, which learns from measured network jitter
- Resampling-based drift correction instead of frame drop/insert
- Encryption (Noise/ChaCha20) and a peer password
- Directory/rendezvous server and NAT hole-punching
- Multi-network bonding
- EBU 3326 SIP mode for Comrex/Tieline interop
- Code signing (Apple Developer ID, Windows Authenticode)
- Saved settings and remote phonebook in the GUI
- Web UI for headless boxes (shared with AirSwitch)
