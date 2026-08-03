- allow plugins that render smth / a window. e.g. something like which-key
- figure out how davinci can cleanly step framewise backwards, and shuttle backwards, we cant currently
- figure out whats causing playback to break after doing a few seeks, or shuttles to the end or fast backwards etc

Done:
- ~~prevent holding a key from lagging/freezing~~ input is drained one batch
  per frame, so a burst of repeats costs one seek (spec §14)
- ~~thumbnails in timeline~~ decoded by the host while the transport is idle
  (spec §15.2)
- ~~relative jump numbers~~ on the ruler's major ticks (spec §3.2)
- ~~audio while shuttling~~
- ~~clip labels over the top of waveform/thumbnail~~
- ~~render typing command~~ the `:` line is view state now (spec §15.3)
- ~~show autocomplete suggestions~~ shown above the line, from the host's
  vocabulary
