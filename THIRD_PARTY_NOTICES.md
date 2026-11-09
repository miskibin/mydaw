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
