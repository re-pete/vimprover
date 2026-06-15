#!/usr/bin/env fish
# Regenerate synthetic test videos in tests/fixtures/.
# All files are 1-second black frames — just enough for ffprobe to read dimensions.

set dir (dirname (status --current-filename))/../tests/fixtures
mkdir -p $dir

ffmpeg -f lavfi -i color=c=black:size=2560x1440:rate=24 -t 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -y $dir/test_2560x1440.mp4
ffmpeg -f lavfi -i color=c=black:size=2160x3840:rate=24 -t 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -y $dir/test_4k_vertical.mp4
ffmpeg -f lavfi -i color=c=black:size=1922x1081:rate=24 -t 1 -c:v ffv1                                      -y $dir/test_1922x1081.mkv
ffmpeg -f lavfi -i color=c=black:size=1920x1080:rate=24 -t 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -y $dir/test_1920x1080.mp4
ffmpeg -f lavfi -i color=c=black:size=1080x1920:rate=24 -t 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -y $dir/test_1080x1920.mp4
ffmpeg -f lavfi -i color=c=black:size=640x480:rate=24   -t 1 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -y $dir/test_640x480.mp4

echo "Done. Created "(count $dir/test_*.{mp4,mkv})" fixture files in $dir"
