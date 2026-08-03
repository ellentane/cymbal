# cymbal

cymbal is a live-coding pattern editor for drum, bass, and lead synthesis. You
type a small pattern language in a terminal editor and cymbal synthesizes the
audio in real time, so you can edit a groove while it plays. It ships with five
built-in voices — kick, snare, hat, bass, and lead — a deterministic offline
renderer, and WAV export.

## Quick start

```sh
cargo run --release -- examples/beat.cym    # live TUI
cargo run --release -- render examples/beat.cym out.wav   # offline render to WAV (120 s by default)
```

Open any `.cym` file to start the live editor. Errors are shown inline and
playback always continues with the last good schedule. `make play` / `make
export` are shortcuts for the examples above; `render` also accepts a length in
seconds: `cargo run --release -- render examples/groove.cym groove.wav 30`.

## Language cheat sheet

```cym
-- comments start with two dashes
tempo 120

let kick = kick()
let bass = bass()
let lead = lead()

loop "beat":
    kick  << "x . . x . . x ."
    bass  << ([c2, f2, g2], "x . . x . . . .")
    lead  << [c4, d4, e4, g4] >> every(4, rev)
```

- `tempo <n>` sets the transport tempo (default 120).
- `let <name> = kick() | snare() | hat() | bass() | lead()` defines a voice.
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

## Keybindings

| Key | Action |
|---|---|
| Ctrl-S | reload the file (swap takes effect at the next bar) |
| Ctrl-= / Ctrl-- | raise / lower tempo |
| Ctrl-E | export the current song to `out.wav` next to the file |
| Ctrl-Q | quit |

## Architecture

Three crates in one workspace:

- `crates/core` (`cymbal-core`) — the language core: lexer, parser, scheduler,
  the five synthesizer voices, and the offline renderer. It never touches a
  soundcard, filesystem, or clock; it consumes a source string and produces
  float samples, which keeps it deterministic and testable.
- `crates/audio` (`cymbal-audio`) — the real-time engine on cpal. The audio
  thread does no allocation, no locks, and no I/O; new schedules arrive through
  a lock-free swap queue and are applied at bar boundaries.
- `crates/tui` (`cymbal`) — the terminal application: ratatui editor with
  syntax highlighting, the keybindings above, and the `render` subcommand.

## Roadmap

The core is platform-agnostic by design; a Web/Wasm frontend (AudioWorklet
plus a SharedArrayBuffer bridge) is the planned phase 2.
