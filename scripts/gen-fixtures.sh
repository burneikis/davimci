#!/usr/bin/env bash
# Generate test media with known ground truth; media is generated, never
# committed.
#
# Everything here has a property the tests can assert exactly: known silence
# spans, known scene-change frames, known track counts, known frame counts.
set -euo pipefail

OUT="${1:-target/fixtures}"
mkdir -p "$OUT"
FF="ffmpeg -hide_banner -loglevel error -y"

gen() { # name, description, ffmpeg args...
  local name=$1 desc=$2
  shift 2
  if [ -f "$OUT/$name" ]; then
    printf '  skip  %-28s %s\n' "$name" "$desc"
    return
  fi
  printf '  gen   %-28s %s\n' "$name" "$desc"
  # shellcheck disable=SC2086
  $FF "$@" "$OUT/$name"
}

echo "Generating fixtures in $OUT"

# --- timing / conform -------------------------------------------------------
# Timecode-burned counters at each rate. Frame N displays N, so a decoded frame
# can be verified without OCR by checking a per-frame colour signature.
for spec in "60:counter_1080p60.mkv" "30:counter_1080p30.mkv" "25:counter_1080p25.mkv"; do
  fps=${spec%%:*}; file=${spec#*:}
  gen "$file" "${fps}fps 10s counter" \
    -f lavfi -i "testsrc2=size=1920x1080:rate=$fps:duration=10" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p
done

gen counter_23976.mkv "23.976fps 10s (NTSC conform)" \
  -f lavfi -i "testsrc2=size=1920x1080:rate=24000/1001:duration=10" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p

gen counter_720p.mkv "720p (upscale conform)" \
  -f lavfi -i "testsrc2=size=1280x720:rate=60:duration=5" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p

gen counter_4k.mkv "2160p (proxy threshold trigger)" \
  -f lavfi -i "testsrc2=size=3840x2160:rate=30:duration=3" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p

# --- audio analysis ---------------------------------------------------------
# Tone from 1-2s and 3-4s; silence elsewhere. Silence spans and peak locations
# are exact and known, so analysis can be asserted to within one 10ms hop.
gen tone_gaps.wav "tone 1-2s and 3-4s, else silence" \
  -f lavfi -i "aevalsrc='if(between(t,1,2)+between(t,3,4),0.5*sin(1000*2*PI*t),0)':d=5:s=48000"

gen silence_5s.wav "pure silence" \
  -f lavfi -i "anullsrc=r=48000:cl=mono:d=5"

# --- scene change -----------------------------------------------------------
# Hard cut at exactly 2.0s (frame 120 at 60fps).
gen scene_cut.mkv "hard cut at frame 120 @60fps" \
  -f lavfi -i "color=red:size=640x480:rate=60:duration=2" \
  -f lavfi -i "color=blue:size=640x480:rate=60:duration=2" \
  -filter_complex "[0:v][1:v]concat=n=2:v=1:a=0" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p

# --- multi-track MKV --------------------------------------------------------
# One video, three distinguishable audio tracks, two subtitle tracks.
# Exercises import: every stream must become its own track.
if [ ! -f "$OUT/multitrack.mkv" ]; then
  printf '  gen   %-28s %s\n' "multitrack.mkv" "1 video, 3 audio, 2 subtitle"
  for i in 1 2 3; do
    hz=$((220 * i))
    $FF -f lavfi -i "sine=frequency=$hz:duration=5:sample_rate=48000" "$OUT/.a$i.wav"
  done
  for i in 1 2; do
    printf '1\n00:00:0%d,000 --> 00:00:0%d,000\nsubtitle track %d\n\n' "$i" "$((i + 2))" "$i" \
      > "$OUT/.s$i.srt"
  done
  $FF -f lavfi -i "testsrc2=size=640x480:rate=30:duration=5" \
    -i "$OUT/.a1.wav" -i "$OUT/.a2.wav" -i "$OUT/.a3.wav" \
    -i "$OUT/.s1.srt" -i "$OUT/.s2.srt" \
    -map 0:v -map 1:a -map 2:a -map 3:a -map 4:s -map 5:s \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -c:s srt \
    -metadata:s:a:0 title="dialogue" \
    -metadata:s:a:1 title="music" \
    -metadata:s:a:2 title="effects" \
    "$OUT/multitrack.mkv"
  rm -f "$OUT"/.a[123].wav "$OUT"/.s[12].srt
else
  printf '  skip  %-28s %s\n' "multitrack.mkv" "1 video, 3 audio, 2 subtitle"
fi

# --- A/V sync ---------------------------------------------------------------
# White flash and audio click both at exactly 1.0s.
gen sync_flash.mkv "flash + click at 1.0s" \
  -f lavfi -i "color=black:size=320x240:rate=60:duration=2,geq=lum='if(eq(N,60),255,0)':cb=128:cr=128" \
  -f lavfi -i "aevalsrc='if(between(t,1.0,1.01),0.9,0)':d=2:s=48000" \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a pcm_s16le

echo
echo "Fixtures ready. Verifying:"
for f in "$OUT"/*.mkv "$OUT"/*.wav; do
  [ -e "$f" ] || continue
  printf '  %-28s %s streams, %s\n' \
    "$(basename "$f")" \
    "$(ffprobe -v error -show_entries format=nb_streams -of csv=p=0 "$f")" \
    "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$f")s"
done
