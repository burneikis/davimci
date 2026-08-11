-- audio: the loudness opinions, over the measurements the editor can take.
--
-- Gain, mute and solo are core: they are what the model holds and what the
-- mix is. Deciding that a track should sit 12 dB under another wherever that
-- other one is loud, or that every clip should land on the same LUFS, is an
-- opinion about how to mix - so `:duck` and `:normalize` live here, and this
-- is where EQ, compression and noise reduction will join them.
--
-- Both read the loudness hops, and nothing is measured unasked, so this asks.

require("davimci.analysis").demand("audio")
