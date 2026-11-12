# Third-party notices

WAVE is licensed MIT OR Apache-2.0. It links the following component whose licence differs.

## LAME (MP3 encoding)

MP3 export uses the [`mp3lame-encoder`](https://crates.io/crates/mp3lame-encoder) Rust bindings
(LGPL-3.0) over [`mp3lame-sys`](https://crates.io/crates/mp3lame-sys), which vendors the
[LAME](https://lame.sourceforge.io/) MP3 encoder source and is built and linked statically.

LAME is Copyright (c) 1999-2017 The LAME Project and earlier authors, and is distributed under the
GNU Lesser General Public License, version 2.0 (<https://www.gnu.org/licenses/old-licenses/lgpl-2.0.html>).
The complete LAME source is available at <https://lame.sourceforge.io/> and, in the exact version
used, in the `mp3lame-sys` crate on crates.io.

LGPL relinking: WAVE is open source and ships its full source, so you may rebuild the application
against a modified LAME (`cargo build` with a patched `mp3lame-sys`). Patents on MP3 have expired.
This notice is not legal advice; check the licence terms before redistributing a binary.

## Standard library samples

The standard library that ships inside WAVE (`crates/engine/library`) is made from
recordings released to the public domain under
[CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/):

* files named `*-808*`: a Roland TR-808 sampled by Michael Fischer (Technopolis), 1994, via
  the [TidalCycles sound library](https://github.com/tidalcycles/sounds-tr808-fischer);
* all other files: the
  [Versilian Community Sample Library](https://github.com/sgossner/VCSL) by Versilian Studios LLC.

The files are edited (silence trimmed, tails faded, peaks normalised, re-encoded as FLAC) by
`scripts/make-factory-library.py`; the exact source of each is pinned in `assets-manifest.json`.
CC0 asks for no credit; it is given here anyway.

## Salamander Grand Piano (WAVE Keys)

The WAVE Keys piano plays recordings from the
[Salamander Grand Piano V3](https://github.com/sfzinstruments/SalamanderGrandPiano) by
Alexander Holm, licensed [CC-BY-3.0](https://creativecommons.org/licenses/by/3.0/). Installers
ship 90 of its recordings, modified: each is cut to 9 seconds with a fade-out (the longest a
note rings in WAVE) and stored as 16-bit FLAC (see `docs/keys.md`). The in-app download installs
the original files unmodified.
Music made with WAVE Keys contains these recordings: credit them as the licence asks.
