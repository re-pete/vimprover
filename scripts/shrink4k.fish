function shrink4k --description "Find and shrink >1080p video files to 1080p using vimprover"
    set -l recursive false
    set -l dry_run false
    set -l auto_yes false
    set -l min_shrink 25
    set -l do_process false
    set -l do_list false
    set -l do_clear false
    set -l queue_file_override ""
    set -l dir ""

    set -l idx 1
    while test $idx -le (count $argv)
        set -l arg $argv[$idx]
        switch $arg
            case -r --recursive
                set recursive true
            case -n --dry-run
                set dry_run true
            case -y --yes
                set auto_yes true
            case --process
                set do_process true
            case --list
                set do_list true
            case --clear
                set do_clear true
            case --queue-file
                set idx (math $idx + 1)
                if test $idx -gt (count $argv)
                    echo "shrink4k: --queue-file requires a value" >&2
                    return 1
                end
                set queue_file_override $argv[$idx]
            case '--queue-file=*'
                set queue_file_override (string replace --regex '^--queue-file=' '' $arg)
            case --min-shrink
                set idx (math $idx + 1)
                if test $idx -gt (count $argv)
                    echo "shrink4k: --min-shrink requires a value" >&2
                    return 1
                end
                set min_shrink $argv[$idx]
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
                echo "Usage: shrink4k [options] [DIRECTORY]"
                echo ""
                echo "Modes:"
                echo "  (default)          Scan DIRECTORY (default: .) and append new candidates to the queue"
                echo "  --list             Show files currently in the queue"
                echo "  --clear            Empty the queue"
                echo "  --process          Process files from the queue one at a time"
                echo ""
                echo "Queue files (default): ~/.local/share/shrink4k/queue  and  .../done"
                echo ""
                echo "Options:"
                echo "  -r, --recursive    Descend into subdirectories (default: top-level only)"
                echo "  -n, --dry-run      Show what would happen without doing it"
                echo "  -y, --yes          Pass --yes to vimprover (skip per-file confirmation)"
                echo "  --min-shrink PCT   Skip files below PCT% pixel reduction (default: 25)"
                echo "  --queue-file PATH  Use PATH instead of ~/.local/share/shrink4k/queue"
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
        set idx (math $idx + 1)
    end

    # Resolve queue and done file paths
    set -l queue_file ""
    set -l done_file ""
    if test -n "$queue_file_override"
        set queue_file $queue_file_override
        set done_file "$queue_file_override.done"
    else
        set -l data_dir "$HOME/.local/share/shrink4k"
        mkdir -p "$data_dir"
        set queue_file "$data_dir/queue"
        set done_file "$data_dir/done"
    end

    if not command -q ffprobe
        echo "shrink4k: ffprobe not found in PATH" >&2
        return 1
    end
    if not command -q vimprover
        echo "shrink4k: vimprover not found in PATH" >&2
        return 1
    end

    # -------------------------------------------------------------------------
    if test "$do_clear" = true
    # CLEAR MODE: empty the queue
    # -------------------------------------------------------------------------

        if not test -s "$queue_file"
            echo "Queue is already empty ($queue_file)"
            return 0
        end
        set -l total (count (grep -v '^[[:space:]]*$' "$queue_file" 2>/dev/null))
        truncate -s 0 "$queue_file"
        echo "Cleared $total file(s) from queue ($queue_file)"

    # -------------------------------------------------------------------------
    else if test "$do_list" = true
    # LIST MODE: show queue contents
    # -------------------------------------------------------------------------

        set -l pending (grep -v '^[[:space:]]*$' "$queue_file" 2>/dev/null)
        set -l total (count $pending)
        if test $total -eq 0
            echo "Queue is empty ($queue_file)"
            return 0
        end
        echo "Queue ($total file(s)) — $queue_file"
        for f in $pending
            echo "  $f"
        end

    # -------------------------------------------------------------------------
    else if test "$do_process" = true
    # PROCESS MODE: read queue, run vimprover, move entries to done file
    # -------------------------------------------------------------------------

        if not test -s "$queue_file"
            echo "Queue is empty or does not exist: $queue_file"
            return 0
        end

        set -l pending (grep -v '^[[:space:]]*$' "$queue_file" 2>/dev/null)
        set -l total (count $pending)

        if test $total -eq 0
            echo "Queue is empty: $queue_file"
            return 0
        end

        set -l yes_flag
        if test "$auto_yes" = true
            set yes_flag --yes
        end

        echo "Processing $total file(s) from $queue_file ..."
        echo ""

        set -l n_done 0
        set -l n_failed 0
        set -l n_missing 0

        for f in $pending
            if not test -e "$f"
                set n_missing (math $n_missing + 1)
                if test "$dry_run" = true
                    echo "missing  $f"
                    echo "         (would be removed from queue)"
                else
                    echo "missing  $f"
                    echo "         (removed from queue)"
                    set -l tmp (mktemp)
                    grep -Fxv -- "$f" "$queue_file" > $tmp 2>/dev/null; true
                    mv $tmp "$queue_file"
                end
                echo ""
                continue
            end

            echo "encode   $f"

            if test "$dry_run" = true
                echo "         (dry-run) vimprover --max-height 1080 --upgrade $yes_flag \"$f\""
                set n_done (math $n_done + 1)
                echo ""
                continue
            end

            vimprover --max-height 1080 --upgrade $yes_flag "$f"
            if test $status -eq 0
                set -l tmp (mktemp)
                grep -Fxv -- "$f" "$queue_file" > $tmp 2>/dev/null; true
                mv $tmp "$queue_file"
                echo "$f" >> "$done_file"
                set n_done (math $n_done + 1)
                echo "         done"
            else
                set n_failed (math $n_failed + 1)
                echo "         FAILED — left in queue for retry" >&2
            end
            echo ""
        end

        set -l remaining (count (grep -v '^[[:space:]]*$' "$queue_file" 2>/dev/null))
        echo "Done."
        if test "$dry_run" = true
            echo "  Would process: $n_done  |  Missing: $n_missing"
        else
            echo "  Processed: $n_done  |  Failed: $n_failed  |  Missing: $n_missing  |  Remaining in queue: $remaining"
        end

    # -------------------------------------------------------------------------
    else
    # DISCOVER MODE: scan directory and append new candidates to the queue file
    # -------------------------------------------------------------------------

        if test -z "$dir"
            set dir .
        else if not test -d "$dir"
            echo "shrink4k: not a directory: $dir" >&2
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

        set -l n_scanned 0
        set -l n_skipped 0
        set -l n_already 0
        set -l n_queued 0

        for f in (find $find_args 2>/dev/null | sort)
            set n_scanned (math $n_scanned + 1)

            set -l dims (ffprobe -v error -select_streams v:0 \
                -show_entries stream=width,height -of csv=p=0 "$f" 2>/dev/null \
                | string trim | string split \n)[1]

            if test -z "$dims"
                set n_skipped (math $n_skipped + 1)
                echo "skip     $f  (no video stream or unreadable)"
                continue
            end

            set -l width  (string split , $dims)[1]
            set -l height (string split , $dims)[2]

            if not string match -qr '^\d+$' $width; or not string match -qr '^\d+$' $height
                set n_skipped (math $n_skipped + 1)
                echo "skip     $f  (couldn't parse dimensions: $dims)"
                continue
            end

            set -l long_side (math "max($width, $height)")
            if test $long_side -le 1920
                set n_skipped (math $n_skipped + 1)
                echo "skip     $f  ($width""x$height, longest side ≤1920)"
                continue
            end

            set -l shrink_pct (math -s0 "(1 - (min($height, 1080) / $height) ^ 2) * 100")
            if test $shrink_pct -lt $min_shrink
                set n_skipped (math $n_skipped + 1)
                echo "skip     $f  ($width""x$height, ~$shrink_pct""% pixel reduction, below $min_shrink""% threshold)"
                continue
            end

            if grep -qxF -- "$f" "$queue_file" 2>/dev/null
                set n_already (math $n_already + 1)
                echo "queued   $f  (already in queue)"
                continue
            end

            if test "$dry_run" = true
                echo "would queue  $f  ($width""x$height, ~$shrink_pct""% reduction)"
            else
                echo "$f" >> "$queue_file"
                echo "queued   $f  ($width""x$height, ~$shrink_pct""% reduction)"
            end
            set n_queued (math $n_queued + 1)
        end

        echo ""
        if test "$dry_run" = true
            echo "Would queue $n_queued new file(s).  Already queued: $n_already  Skipped: $n_skipped"
        else
            set -l total_pending (count (grep -v '^[[:space:]]*$' "$queue_file" 2>/dev/null))
            echo "Queued $n_queued new file(s).  Already queued: $n_already  Skipped: $n_skipped"
            if test $total_pending -gt 0
                echo "Queue ($total_pending total): $queue_file"
                echo "Run 'shrink4k --process' to encode."
            end
        end

    end
end
