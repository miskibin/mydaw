WAVE Keys: the piano's recordings go here (90 FLAC files, about 23 MB) and are shipped
inside the installer. They are not in Git: `node scripts/fetch-keys.mjs` downloads the
originals (138 MB, each checked against the SHA-256 pinned in assets-manifest.json) and
cuts them to the 9 seconds the app plays, as 16-bit FLAC. The release workflow runs it
before the build. A build without them still works: the
app then offers to download the piano into the sample library.

Salamander Grand Piano V3 by Alexander Holm, licensed CC-BY-3.0.
https://github.com/sfzinstruments/SalamanderGrandPiano
