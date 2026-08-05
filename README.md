# cymbal

cymbal is a live-coding pattern editor for drum, bass, and lead synthesis. You
type a small pattern language in a terminal editor and cymbal synthesizes the
audio in real time, so you can edit a groove while it plays. It ships with five
synthesizer voices — kick, snare, hat, bass, and lead — a bundled sample kit,
a deterministic offline renderer, WAV/stem export, MIDI out, and a Web/Wasm
frontend.

## Quick start

```sh
cargo run --release -- examples/kit.cym    # live TUI
cargo run --release -- render examples/beat.cym out.wav   # offline render to WAV (120 s by default)
cargo run --release -- render --f32 examples/beat.cym out.wav   # 32-bit float WAV
cargo run --release -- render --tracks examples/groove.cym stems   # per-loop dry stems + master mix
cargo run --release -- --midi examples/beat.cym   # live TUI with MIDI note/clock out
```

Open any `.cym` file to start the live editor. Errors are shown inline and
playback always continues with the last good schedule. `make play` / `make
export` are shortcuts for the examples above; `render` also accepts a length in
seconds: `cargo run --release -- render examples/groove.cym groove.wav 30`.
`--midi [port]` opens the named MIDI output port (the first available one when
omitted) and streams note-on/off plus a 24 PPQN clock; `render --tracks`
writes one dry stem per loop into the output directory alongside the full mix
in `master.wav`.

## Language cheat sheet

```cym
-- comments start with two dashes
tempo 120

let kick = kick()
let bass = bass()
let lead = lead()
let clap = sample "clap"

loop "beat":
    kick  << "x . . x . . x ." vel=0.9 bass=0.3
    bass  << ([c2, f2, g2], "x . . x . . . .") pan=-0.4 swing=0.25
    lead  << [c4, d4, e4, g4] >> every(4, rev) delay=0.4
    clap  << "x . x!0.5 x@2 . x ." reverb=0.4
    hat   << "x . x . x . x ." pan=-0.5:0.5
```

- `tempo <n>` sets the transport tempo (default 120).
- `let <name> = kick() | snare() | hat() | bass() | lead()` defines a voice.
- `let <name> = sample "file.wav"` defines a sample voice from a WAV file;
  paths are relative to the `.cym` file. A bare name like `sample "kick"`
  falls back to the bundled kit: `kick`, `snare`, `hat`, `clap`, `loop`.
- `loop "<name>":` opens an infinitely repeating loop block; its body binds
  patterns to voices with `<<`.
- A pattern string's length is the subdivision: `"x . . x . . x ."` (8 chars)
  is eighth notes, `"x . x ."` (4 chars) is quarters. Whitespace is ignored;
  `x` hits, `.` rests. Different-length patterns phase against each other, so
  polyrhythms fall out by construction.
- `[c4, d4, e4, g4]` is a note array — evenly spaced triggers, one pitch each.
- `([c2, f2, g2], "x . . x . . . .")` pairs pitches with a custom rhythm.
- `>> rev` reverses a pattern's steps; `>> every(n, rev)` reverses every nth
  cycle.
- After the pattern (and any `>>` combinators), a bind takes mix parameters:
  `pan=-0.4` (-1 left .. 1 right), `vel=0.9` (scales hit velocity),
  `delay=0.25` (delay send), `reverb=0.4` (reverb send), `bass=0.5` /
  `treble=0.5` (per-voice low/high shelves), `comp=0.3` (per-voice
  compressor), `swing=0.25` (delays the odd 8th steps). `vel`, `delay`,
  `reverb`, `bass`, `treble`, `comp` accept 0..=1; `swing` 0..=0.5.
- `pan=`, `vel=`, `delay=`, and `reverb=` also accept a ramp `a:b` — the value
  sweeps from `a` to `b` across each bar.
- Sample voices accept regions: `start=0.25 end=0.75` play a slice of the
  file, `dur=0.2` overrides the trigger length in seconds, and `cycle=1`
  loops the region for the trigger's duration.
- Inside a pattern string, `x!0.5` sets a per-hit velocity (0..=1) and `x@2`
  transposes a hit by 2 semitones. `@n` works on every voice: pitched voices
  transpose, kick/snare shift their body pitch, hats ignore it.

## Keybindings

| Key | Action |
|---|---|
| Ctrl-S | reload the file — only changed loops rebuild, notes already sounding on unchanged loops keep playing |
| Ctrl-= / Ctrl-- | raise / lower tempo; forces a full reload of every loop |
| Alt-R / Alt-H / Alt-[ / Alt-] | reverse / half-speed / rotate the pattern on the cursor line, then reload at the next bar |
| Ctrl-R | toggle recording — writes `recording-<timestamp>.wav` next to the file, plus `recording-<timestamp>-<loop>.wav` per loop present at record start; shows `REC mm:ss` in the status bar |
| Ctrl-E | export the current song to `out.wav` next to the file |
| Ctrl-Q | quit |

## Live reload

Reloads are per-loop: on Ctrl-S each loop gets a generation id and only the
loops whose contents changed are rebuilt and swapped into the running schedule.
Notes already sounding on unchanged loops play out uninterrupted, so you can
audition edits without cutting a drum fill. Changing tempo (Ctrl-= / Ctrl--)
marks every loop dirty and rebuilds all of them.

## Architecture

Four crates in one workspace:

- `crates/core` (`cymbal-core`) — the language core: lexer, parser, scheduler,
  the five synthesizer voices, and the offline renderer. It never touches a
  soundcard, filesystem, or clock; it consumes a source string and produces
  float samples, which keeps it deterministic and testable.
- `crates/audio` (`cymbal-audio`) — the real-time engine on cpal. The audio
  thread does no allocation, no locks, and no I/O; new schedules arrive through
  a lock-free swap queue and are applied at bar boundaries. The core renders at
  48 kHz and a linear resampler converts to the device rate when it differs.
- `crates/tui` (`cymbal`) — the terminal application: ratatui editor with
  syntax highlighting, the keybindings above, and the `render` subcommand.
- `crates/wasm` (`cymbal-wasm`) — the Web/Wasm frontend: a wasm-bindgen API
  (compile, offline render, timeline serialization) plus a `no_mangle` engine
  module that runs the core voices inside an AudioWorklet. The demo page lives
  in `crates/wasm/web`; serve that directory after building the crate for
  `wasm32-unknown-unknown` and running `wasm-bindgen --target web`.

## Roadmap

The Web/Wasm frontend shipped with v1.2; wasm v1 runs the synth voices without
the FX sends or sample voices. Planned follow-ups: MIDI start/stop messages
and tighter clock, per-loop recording for loops added mid-recording, and
further audio-thread allocation discipline for very long timelines.

## Platforms

- Linux (PipeWire/ALSA) — primary target
- macOS (CoreAudio/CoreMIDI) — CI-tested
- Windows (WASAPI/WinMM) — CI-tested

Device sample rates other than 48 kHz are supported via resampling on the
audio thread; recordings are always 48 kHz.
