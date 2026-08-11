-- proxies: transcode heavy sources to something that scrubs, on import.
--
-- The editor knows how to make a proxy and how to substitute one on the way
-- to the preview; whether a file deserves one is a workflow opinion, so it
-- lives here. With this plugin off nothing is transcoded behind your back,
-- and `:set proxy on` still turns the mechanism on by hand.
--
-- The thresholds are the interesting part: raise `max_native_height` if your
-- machine scrubs 4K happily, or add a codec that seeks badly for you.

require("davimci.proxy").setup({
  auto = true,
  height = 540,
  -- ffmpeg spells ProRes Proxy as `prores_ks` at profile 0; there is no
  -- `prores_proxy` encoder to ask for.
  codec = "prores_ks",
  max_native_height = 1080,
  expensive_codecs = { "hevc", "h265", "vp9", "av1" },
  max_native_bit_depth = 8,
})
