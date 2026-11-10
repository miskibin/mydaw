WAVE Keys: the piano's recordings go here (90 FLAC files, 138 MB) and are shipped
inside the installer. They are not in Git: `node scripts/fetch-keys.mjs` downloads
them, checking every file against the SHA-256 pinned in assets-manifest.json. The
release workflow runs it before the build. A build without them still works: the
app then offers to download the piano into the sample library.

Salamander Grand Piano V3 by Alexander Holm, licensed CC-BY-3.0.
https://github.com/sfzinstruments/SalamanderGrandPiano
