# hooks/wrapper.bash — activate the L4 git-wrapper in this shell.
#
# Source from your bash/zsh init so that interactive `git` commands are
# routed through hooks/git-wrapper.sh. Without this step git-wrapper.sh
# exists on disk but nothing invokes it — L4 of the 4-layer matrix is
# then inactive and the `-c user.email=…` / `git notes` / external-cwd
# write paths become unguarded.
#
# Example installation (bash):
#   echo "source /abs/path/to/wperf/hooks/wrapper.bash" >> ~/.bashrc
#
# The wrapper re-execs `/usr/bin/git` for read-only subcommands, so
# interactive workflows are not degraded.

_wperf_wrapper_dir="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &>/dev/null && pwd )"

if [[ -x "${_wperf_wrapper_dir}/git-wrapper.sh" ]]; then
    git() {
        "${_wperf_wrapper_dir}/git-wrapper.sh" "$@"
    }
else
    printf 'wperf wrapper.bash: %s not executable; L4 not activated\n' \
        "${_wperf_wrapper_dir}/git-wrapper.sh" >&2
fi
