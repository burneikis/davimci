# Human
- the audio levels are rendering the same for all tracks, even when each track has different audio/levels
- allow plugins that render smth / a window. e.g. something like which-key
- backwards shuttle audio?
- different speed shuttle audio
- backwards shuttle lag, we can skip a lot of frames when going backwards shuttling speed
- figure out how davinci resolve can cleanly step framewise backwards
- figure out whats causing playback to break after doing a few seeks, or shuttles to the end or fast backwards etc
- refactor all for clean code
- write docs, guide for the codebase and how to learn it
- plugin support for gui windows
- use clippy on pedantic / nursery

# AI
- ask for `.davimci.lua` trust in the window rather than on the terminal, once the app has a modal path
- `:set proxy on|off` (spec 10.3) is named in the spec but not in the `:set`
  registry; proxies have no runtime switch yet
- a burned-in subtitle is not asserted by a pixel diff: MLT's text producers
  need a display, so the slow test only proves the text stayed out of the
  streams and the sidecar
- spec 6 says `V` (visual-line) snaps the selection to whole clips; nothing
  implements the snap, so `V` behaves like `v` until a motion extends it
- `an_exported_file_has_the_duration_of_the_timeline` fails on a 5s timeline:
  the file comes out 5.088s. Pre-dates the hardening pass; the export writes
  a few frames more than the timeline holds
