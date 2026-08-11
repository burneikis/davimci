-- presets: the export catalogue.
--
-- Exporting is core; a list of container-and-codec pairings someone found
-- useful is registration data, exactly as the transition catalogue is. The
-- editor keeps one fallback, `mkv`, so an export is always possible; every
-- other name here is a registration through the API a third-party preset
-- uses, and any of them can be overridden by defining the same name again.

local export = require("davimci.export")

export.preset("mkv_h265", { container = "mkv", video_codec = "h265", audio_codec = "flac" })
export.preset("mp4", { container = "mp4", video_codec = "h264", audio_codec = "aac" })
export.preset("mp4_h265", { container = "mp4", video_codec = "h265", audio_codec = "aac" })
export.preset("webm", { container = "webm", video_codec = "vp9", audio_codec = "opus" })
export.preset("prores", { container = "mov", video_codec = "prores", audio_codec = "pcm" })
