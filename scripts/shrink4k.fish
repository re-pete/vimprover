function shrink4k --description "Find and shrink >1080p video files to 1080p using vimprover"
    set -l recursive false
    set -l dry_run false
    set -l auto_yes false
    set -l min_shrink 25
    set -l dir ""

    set -l i 1
    while test $i -le (count $argv)
        set -l arg $argv[$i]
        switch $arg
            case -r --recursive
                set recursive true
            case -n --dry-run
                set dry_run true
            case -y --yes
                set auto_yes true
            case --min-shrink
                set i (math $i + 1)
                if test $i -gt (count $argv)
                    echo "shrink4k: --min-shrink requires a value" >&2
                    return 1
                end
                set min_shrink $argv[$i]
                if not string match -qr '^\d+$' $min_shrink; or test $min_shrink -gt 100
                    echo "shrink4k: --min-shrink must be 0–100" >&2
                    return 1
                end
            case '--min-shrink=*'
                set min_shrink (string replace --regex '^--min-shrink=' '' $arg)
                if not string match -qr '^\d+$' $min_shrink; or test $min_shrink -gt 100
                    echo "shrink4k: --min-shrink must be 0–100" >&2
                    return 1
                end
            case --help -h
                echo "Usage: shrink4k [options] DIRECTORY"
                echo ""
                echo "  -r, --recursive        Descend into subdirectories (default: top-level only)"
                echo "  -n, --dry-run          Print what would run without executing vimprover"
                echo "  -y, --yes              Pass --yes to vimprover (skip per-file confirmation)"
                echo "  --min-shrink PCT       Skip files where estimated pixel reduction < PCT% (default: 25)"
                return 0
            case '-*'
                echo "shrink4k: unknown option: $arg" >&2
                echo "Usage: shrink4k [options] DIRECTORY  (--help for full usage)" >&2
                return 1
            case '*'
                if test -n "$dir"
                    echo "shrink4k: unexpected argument: $arg" >&2
                    return 1
                end
                set dir $arg
        end
        set i (math $i + 1)
    end

    if test -z "$dir"
        echo "shrink4k: DIRECTORY is required" >&2
        echo "Usage: shrink4k [options] DIRECTORY  (--help for full usage)" >&2
        return 1
    end

    if not test -d "$dir"
        echo "shrink4k: not a directory: $dir" >&2
        return 1
    end

    if not command -q ffprobe
        echo "shrink4k: ffprobe not found in PATH" >&2
        return 1
    end

    if not command -q vimprover
        echo "shrink4k: vimprover not found in PATH" >&2
        return 1
    end

    set -l ext_filters \
        -iname "*.mkv" -o \
        -iname "*.mp4" -o \
        -iname "*.avi" -o \
        -iname "*.mov" -o \
        -iname "*.wmv" -o \
        -iname "*.flv" -o \
        -iname "*.m4v" -o \
        -iname "*.webm" -o \
        -iname "*.vob"

    set -l find_args $dir -type f \( $ext_filters \)
    if test "$recursive" = false
        set find_args $dir -maxdepth 1 -type f \( $ext_filters \)
    end

    set -l yes_flag
    if test "$auto_yes" = true
        set yes_flag --yes
    end

    set -l n_scanned 0
    set -l n_skipped 0
    set -l n_processed 0
    set -l n_failed 0
    set -l candidates

    for f in (find $find_args 2>/dev/null | sort)
        set n_scanned (math $n_scanned + 1)
        set dims (ffprobe -v error -select_streams v:0 \
            -show_entries stream=width,height -of csv=p=0 "$f" 2>/dev/null \
            | string trim | string split \n)[1]

        if test -z "$dims"
            set n_skipped (math $n_skipped + 1)
            echo "skip  $f  (no video stream or unreadable)"
            continue
        end

        set width  (string split , $dims)[1]
        set height (string split , $dims)[2]

        if not string match -qr '^\d+$' $width; or not string match -qr '^\d+$' $height
            set n_skipped (math $n_skipped + 1)
            echo "skip  $f  (couldn't parse dimensions: $dims)"
            continue
        end

        # Flag only when the longest dimension exceeds 1920.
        # This handles portrait video correctly: a 1080×1920 clip has max=1920 and is skipped.
        set long_side (math "max($width, $height)")
        if test $long_side -le 1920
            set n_skipped (math $n_skipped + 1)
            echo "skip  $f  ($width""x$height, longest side ≤1920)"
            continue
        end

        # Estimate pixel-area reduction from --max-height 1080.
        # Both dimensions scale by (min(height,1080)/height), so area scales by that squared.
        set -l shrink_pct (math -s0 "(1 - (min($height, 1080) / $height) ^ 2) * 100")
        if test $shrink_pct -lt $min_shrink
            set n_skipped (math $n_skipped + 1)
            echo "skip  $f  ($width""x$height, ~$shrink_pct""% pixel reduction, below $min_shrink""% threshold)"
            continue
        end

        set candidates $candidates "$f:$width:$height"
    end

    set -l total (count $candidates)

    if test $total -eq 0
        echo "No files above 1920px (longest side) found in $dir."
        echo "Scanned $n_scanned file(s), skipped $n_skipped."
        return 0
    end

    echo "Found $total file(s) above 1080p. Processing..."
    echo ""

    set -l i 0
    for entry in $candidates
        set i (math $i + 1)
        # format is "path:width:height"; path may contain colons, so peel from the right
        set -l parts (string split : $entry)
        set height $parts[-1]
        set width  $parts[-2]
        set f (string join : $parts[1..-3])

        echo "[$i/$total] $f ($width""x$height → 1080p)"

        if test "$dry_run" = true
            echo "       (dry-run) vimprover --max-height 1080 --upgrade $yes_flag \"$f\""
            set n_processed (math $n_processed + 1)
        else
            vimprover --max-height 1080 --upgrade $yes_flag "$f"
            if test $status -eq 0
                set n_processed (math $n_processed + 1)
            else
                set n_failed (math $n_failed + 1)
                echo "       FAILED (vimprover exited non-zero)" >&2
            end
        end
        echo ""
    end

    echo "Done."
    if test "$dry_run" = true
        echo "  Would process: $n_processed  |  Skipped: $n_skipped"
    else
        echo "  Processed: $n_processed  |  Failed: $n_failed  |  Skipped: $n_skipped"
    end
end
