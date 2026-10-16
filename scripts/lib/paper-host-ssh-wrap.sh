# Shared remote bootstrap for paper-host operator scripts.
# Sourced only from heredocs running on the SSH target — not executed directly.
#
# When the SSH login is root (common for Host jarvis) but agents run as a normal
# user with systemd --user, re-exec the remainder of the script stdin as that user
# with HOME, PATH, XDG_RUNTIME_DIR, and REPO set.
paper_host_reexec_if_root() {
  if [[ "$(id -un)" != root ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
    return 0
  fi

  local u="${RUN_USER:-}"
  if [[ -z "${u}" ]] && id jarvis &>/dev/null; then
    u=jarvis
  fi
  if [[ -z "${u}" ]]; then
    echo "paper-host: SSH is root but no RUN_USER and no 'jarvis' account; set PAPER_HOST_RUN_USER" >&2
    return 1
  fi

  local h uid repo
  h="$(getent passwd "${u}" | cut -d: -f6)"
  uid="$(id -u "${u}")"
  repo="${REPO:-${h}/projects/schwabinvestbot}"

  exec sudo -u "${u}" env \
    HOME="${h}" \
    XDG_RUNTIME_DIR="/run/user/${uid}" \
    REPO="${repo}" \
    OPTIONS_RULES="${OPTIONS_RULES:-}" \
    SWING_RULES="${SWING_RULES:-}" \
    PATH="${h}/.cargo/bin:${PATH}" \
    bash -s
}
