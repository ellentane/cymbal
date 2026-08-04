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
let perc = sample "drums.wav"

loop "beat":
    kick  << "x . . x . . x ." vel=0.9
    bass  << ([c2, f2, g2], "x . . x . . . .") pan=-0.4
    lead  << [c4, d4, e4, g4] >> every(4, rev) delay=0.4
    perc  << "x . x!0.5 x@2 . x ." reverb=0.4
```

- `tempo <n>` sets the transport tempo (default 120).
- `let <name> = kick() | snare() | hat() | bass() | lead()` defines a voice.
- `let <name> = sample "file.wav"` defines a sample voice from a WAV file;
  paths are relative to the `.cym` file.
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
  `delay=0.25` (delay send), `reverb=0.4` (reverb send). `vel`, `delay`, and
  `reverb` accept 0..=1.
- Inside a pattern string, `x!0.5` sets a per-hit velocity (0..=1) and `x@2`
  transposes a hit by 2 semitones. `@n` is only valid on sample voices.

## Keybindings

| Key | Action |
|---|---|
| Ctrl-S | reload the file — only changed loops rebuild, notes already sounding on unchanged loops keep playing |
| Ctrl-= / Ctrl-- | raise / lower tempo; forces a full reload of every loop |
| Ctrl-R | toggle recording — writes `recording.wav` next to the file, shows `REC mm:ss` in the status bar |
| Ctrl-E | export the current song to `out.wav` next to the file |
| Ctrl-Q | quit |

## Live reload

Reloads are per-loop: on Ctrl-S each loop gets a generation id and only the
loops whose contents changed are rebuilt and swapped into the running schedule.
Notes already sounding on unchanged loops play out uninterrupted, so you can
audition edits without cutting a drum fill. Changing tempo (Ctrl-= / Ctrl--)
marks every loop dirty and rebuilds all of them.

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
