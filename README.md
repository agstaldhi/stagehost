# StageHost RS

A Windows plugin host that sits between two audio devices and runs VST3, VST2, and CLAP effects on the audio passing through. I built it to put a plugin chain on my microphone inside Voicemeeter.

The idea comes from two programs. LightHost is a small tray app that runs plugins between an input and an output device. Cantabile organizes plugins into racks, song states, and set lists for live playing. StageHost RS aims for LightHost's small footprint and borrows some of Cantabile's live features.

Status: personal project, Windows only, no prebuilt releases. Build it from source (see [Building](#building)).

## My setup: plugins on a Voicemeeter mic

Voicemeeter has no plugin slots. StageHost RS takes audio from one Voicemeeter virtual output, runs it through a rack of plugins, and sends the result back into a Voicemeeter virtual input.

```text
Mic -> hardware strip -> bus B1
"Voicemeeter Out B1"  -> StageHost RS input
StageHost RS output   -> "Voicemeeter Input" (VAIO virtual strip)
VAIO virtual strip    -> bus B2 -> Discord, OBS, browser ("Voicemeeter Out B2")
```

This needs Voicemeeter Banana or Potato, because the routing uses two virtual buses.

1. On the mic's hardware strip in Voicemeeter, enable B1 and nothing else.
2. In StageHost RS, open **Audio > Audio & Plugin Options...**. Pick `Voicemeeter Out B1` as the input and `Voicemeeter Input` as the output. My setup runs these as WASAPI devices at 48 kHz with a 256-frame buffer.
3. Insert your plugins into a rack.
4. In Voicemeeter, route the VAIO virtual strip to B2 (add A1 if you want to hear yourself). Keep its B1 button off.
5. In your apps, select `Voicemeeter Out B2` as the microphone.

> [!WARNING]
> If the VAIO virtual strip also sends to B1, the processed signal goes back into StageHost RS and feeds back. Keep B1 off on that strip.

## Other setups

I have only tested the Voicemeeter mic chain above. The feature set was written with live playing in mind, so these setups should also work:

- A guitar or keyboard rig: interface input, effect racks, the tuner, a set list you step through with PageUp/PageDown, and Panic to clear reverb or delay tails between songs. Use the interface's ASIO driver if it ships one.
- Processing another Voicemeeter bus, such as an EQ on the bus that feeds your headphones, with the same capture-and-return routing.
- Mic processing without Voicemeeter: capture a physical input and output to a virtual cable driver that your apps use as their microphone.

## Features

### Audio

- WASAPI or ASIO output with input capture. With ASIO you choose the input and output channel.
- Input channel modes: stereo, left channel to both sides (`L->ST`), right channel to both sides (`R->ST`), or mono sum (`SUM`). The input also has a gain control and a mute.
- The audio thread runs in the Windows MMCSS "Pro Audio" class with power throttling disabled and denormals flushed to zero. When more than one rack is active, the racks are processed in parallel on a small worker pool.
- A 440 Hz test tone at -18 dBFS for checking the signal path, and a Panic button that resets effect tails without stopping the stream.
- A chromatic tuner on the live input.

### Plugins

- VST3 (`.vst3`), CLAP (`.clap`), and 64-bit VST2 (`.dll`). 32-bit plugins are not supported.
- Five built-in effects: chorus/delay, gain with limiter, 3-band EQ, gate/compressor, and reverb.
- The scanner opens each plugin file in a separate child process with a 10-second timeout. A plugin that crashes or hangs during the scan gets skipped and logged, and the host keeps running. Scan results are cached between runs.
- Plugin editors open in their own native windows. Each slot also has a parameter drawer with a search filter.

### Racks and routing

- Each rack has gain, pan, mid/side width (`0.0` mono to `2.0` wide), mute, solo, and an input switch. Turn the input off for racks that only receive aux sends.
- Post-fader aux sends from one rack to another.
- Sidechain input for plugins that accept one, fed from another rack (pre- or post-fader) or from the live input.
- Each plugin slot has enable, bypass, gain, pan, and dry/wet mix.
- Racks and slots can be duplicated together with their plugin state.

### Sessions and live use

- Sub-sessions store snapshots of rack and slot settings. Switch between them with Alt+1 to Alt+9.
- Set lists hold a sequence of session files. Step through them with PageUp/PageDown or the Prev/Next buttons, with optional autosave when the song changes.
- Undo and redo for session edits.
- The key bindings manager maps keys to actions: toggle or bypass a slot, open a plugin editor, set a parameter value, mute, solo, or enable racks, toggle a send, or load a session file. Bindings are system-wide, so they work while another program has focus or while StageHost RS is in the tray.

### Running in the background

- Closing the window can send StageHost RS to the system tray. Audio, plugins, and key bindings keep running, and the UI and its OpenGL context are released until you reopen the window from the tray icon.
- An option registers StageHost RS to start with Windows for the current user, and another starts it directly in the tray.

## Building

Requirements:

- 64-bit Windows (development happens on Windows 11)
- Rust stable with the MSVC toolchain, installed through [rustup](https://rustup.rs)

```powershell
# Build and start the release binary
cargo build -p ui-app --release
.\target\release\stagehost.exe

# Run the test suite
cargo test --workspace
```

### Command-line flags

| Flag | Effect |
|---|---|
| `--tray`, `--minimized` | Start hidden in the system tray. |
| `--autostart` | Used by the Windows startup entry. Switches the working directory to the executable's folder, and starts in the tray when that option is enabled. |
| `--scan-plugin <path>` | Internal. The child-process mode the plugin scanner uses. |

## Keyboard shortcuts

These work while the window has focus. Shortcuts you define in the key bindings manager work everywhere.

| Keys | Action |
|---|---|
| Ctrl+S | Save session |
| Ctrl+Z | Undo |
| Ctrl+Y or Ctrl+Shift+Z | Redo |
| Alt+1 to Alt+9 | Switch sub-session |
| PageUp / PageDown | Previous / next set list entry |
| Escape | Close the open dialog |

## Files

| What | Where |
|---|---|
| Settings, plugin cache, autosaves | `%APPDATA%\StageHostRS` |
| Sessions | `*.stagehost.json` |
| Set lists | `*.stagelist.json` |
| Full settings export | `*.stagehost-settings.json` |

## Project layout

| Crate | Contents |
|---|---|
| `crates/audio-core` | WASAPI (through `cpal`) and native ASIO backends, the rack/send DSP graph, MMCSS and worker-pool threading, and the tuner. |
| `crates/plugin-host` | VST3, VST2, and CLAP hosting, the built-in effects, and the out-of-process scanner. |
| `crates/session-core` | Session, rack, sub-session, set list, and key binding models, undo history, and JSON persistence. |
| `crates/ui-app` | The `stagehost` executable: `egui` interface, tray icon, global key hook, plugin editor windows, and Windows startup registration. |

## License

No license has been chosen yet, so all rights are reserved for now.
