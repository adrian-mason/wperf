# hooks/wrapper.fish — activate the L4 git-wrapper in this fish shell.
#
# Source from your fish init so that interactive `git` commands are
# routed through hooks/git-wrapper.sh. Without this step git-wrapper.sh
# exists on disk but nothing invokes it — L4 of the 4-layer matrix is
# then inactive and the `-c user.email=…` / `git notes` / external-cwd
# write paths become unguarded.
#
# Example installation (fish):
#   echo "source /abs/path/to/wperf/hooks/wrapper.fish" >> \
#       ~/.config/fish/config.fish
#
# The wrapper re-execs `/usr/bin/git` for read-only subcommands, so
# interactive workflows are not degraded.

set -g _WPERF_WRAPPER (dirname (status --current-filename))/git-wrapper.sh

if test -x "$_WPERF_WRAPPER"
    function git --wraps=git --description 'wperf L4 git-wrapper'
        command "$_WPERF_WRAPPER" $argv
    end
else
    echo "wperf wrapper.fish: $_WPERF_WRAPPER not executable; L4 not activated" >&2
end
