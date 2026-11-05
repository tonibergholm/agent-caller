#!/usr/bin/env bash
# Install smoke test for the agent-caller Arch packages. Run it as a normal
# user on the machine where the packages are installed:
#
#   packaging/smoke-test.sh [--live] [--prefix DIR]
#
#   --live       also check the running service against your real session: the
#                user unit must be active (systemctl --user is-active) and
#                `acd-inbox status` must report the service running and the
#                socket answering. It needs the user bus, so it only makes
#                sense on the Omarchy machine, logged in. The exit status of
#                `acd-inbox status` alone is not health: it is 0 when the
#                service is stopped.
#   --prefix DIR where the packages put files (default /usr).
#
# It exits 0 only when every check passes, and 1 otherwise. It never touches
# your real ~/.config/acd, tokens or keyring: `acd init` runs in a temporary
# HOME that is removed on exit. Token values are never printed.
#
# Packages missing on purpose are reported as SKIP, not as a failure:
# agent-caller (acd) and agent-caller-omarchy (acd-inbox) install separately.
set -euo pipefail

prefix=/usr
live=0
while [ "$#" -gt 0 ]; do
	case "$1" in
	--live) live=1 ;;
	--prefix)
		[ "$#" -ge 2 ] || { echo "smoke-test: --prefix needs a value" >&2; exit 2; }
		prefix=$2
		shift
		;;
	-h | --help)
		awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
		exit 0
		;;
	*) echo "smoke-test: unknown argument: $1" >&2; exit 2 ;;
	esac
	shift
done

fails=0
pass() { printf 'ok    %s\n' "$1"; }
skip() { printf 'SKIP  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }
check() { # description, command...
	local d=$1
	shift
	local out
	if out=$("$@" 2>&1); then pass "$d"; else fail "$d"; printf '%s\n' "$out" | head -5 | sed 's/^/      /'; fi
}

if [ "$(id -u)" -eq 0 ]; then
	echo "smoke-test: run as a normal user, not root" >&2
	exit 2
fi

work=$(mktemp -d) || exit 2
trap 'rm -rf "$work"' EXIT
mode_of() { stat -c %a "$1"; }
other_bits_clear() { # path: no group or world permission bits
	case "$(mode_of "$1")" in *00) return 0 ;; *) return 1 ;; esac
}

acd_bin=$prefix/bin/acd
inbox_bin=$prefix/bin/acd-inbox
unit_dir=$prefix/lib/systemd/user
share=$prefix/share/agent-caller
# The -git packages (agent-caller-git, agent-caller-omarchy-git) install the
# licence under their own name.
# shellcheck disable=SC2329 # called through check()
licence() { test -s "$prefix/share/licenses/$1/LICENSE" || test -s "$prefix/share/licenses/$1-git/LICENSE"; }

# --- acd (package agent-caller) ---
if [ -x "$acd_bin" ]; then
	acd_ver=$("$acd_bin" --version 2>/dev/null) || acd_ver=
	case "$acd_ver" in "acd "*) pass "acd --version: $acd_ver" ;; *) fail "acd --version gave: '$acd_ver'" ;; esac
	check "licence installed for agent-caller" licence agent-caller
	check "acd.service verifies" systemd-analyze verify "$unit_dir/acd.service"

	home=$work/home
	mkdir -p "$home"
	if HOME=$home ACD_CONFIG_DIR=$home/.config/acd "$acd_bin" init >"$work/init.out" 2>&1; then
		pass "acd init in a temporary HOME"
		cfg=$home/.config/acd
		check "config dir is owner-only" other_bits_clear "$cfg"
		bad=0
		for f in client.token hook-tokens.json; do
			[ -f "$cfg/$f" ] || { fail "acd init wrote $f"; bad=1; continue; }
			other_bits_clear "$cfg/$f" || { fail "$f is group or world accessible"; bad=1; }
		done
		[ "$bad" -eq 0 ] && pass "tokens are owner-only"
		# The packaged unit is what `service install` writes (home -> %h).
		printed=$(HOME=$home ACD_CONFIG_DIR=$cfg "$acd_bin" service install --print 2>/dev/null) || printed=
		want=$(printf '%s\n' "$printed" | sed -e "s|$(cd "$cfg" && pwd -P)|%h/.config/acd|" -e "s|$(command -v "$acd_bin")|$acd_bin|")
		if [ "$want" = "$(cat "$unit_dir/acd.service")" ]; then pass "acd.service matches 'acd service install --print'"; else fail "acd.service differs from 'acd service install --print'"; fi
	else
		fail "acd init failed (see $work/init.out)"
		sed 's/[A-Za-z0-9+/=_-]\{24,\}/<redacted>/g' "$work/init.out" | head -5
	fi
else
	skip "acd not installed at $acd_bin (package agent-caller)"
fi

# --- acd-inbox (package agent-caller-omarchy) ---
if [ -x "$inbox_bin" ]; then
	inbox_ver=$("$inbox_bin" --version 2>/dev/null) || inbox_ver=
	case "$inbox_ver" in "acd-inbox "*) pass "acd-inbox --version: $inbox_ver" ;; *) fail "acd-inbox --version gave: '$inbox_ver'" ;; esac
	if [ -n "${acd_ver:-}" ] && [ "${acd_ver#acd }" != "${inbox_ver#acd-inbox }" ]; then
		fail "acd and acd-inbox versions differ ($acd_ver, $inbox_ver)"
	fi
	check "licence installed for agent-caller-omarchy" licence agent-caller-omarchy
	check "acd-inbox.service verifies" systemd-analyze verify "$unit_dir/acd-inbox.service"
	id=tonibergholm.agentcaller
	ref=$share/omarchy/$id
	for f in manifest.json BarWidget.qml Panel.qml Service.qml model.mjs; do
		[ -s "$ref/$f" ] || fail "reference plugin file missing: $ref/$f"
	done

	# Per-user state, if `acd-inbox install` has run. A user who has not run
	# it yet gets SKIP; stale copies after an upgrade are a FAIL.
	user_bin=$HOME/.local/bin/acd-inbox
	if [ -e "$user_bin" ]; then
		if cmp -s "$user_bin" "$inbox_bin"; then pass "the per-user acd-inbox in .local/bin is the installed binary"; else fail "the per-user acd-inbox in .local/bin is stale: run $inbox_bin install"; fi
	else
		skip "acd-inbox install has not been run by this user (no $user_bin)"
	fi
	# `acd-inbox install` by bare name runs whatever comes first in PATH. A
	# stale per-user copy there would reinstall its own, older plugin files.
	on_path=$(command -v acd-inbox || true)
	if [ -n "$on_path" ] && [ "$on_path" != "$inbox_bin" ] && ! cmp -s "$on_path" "$inbox_bin"; then
		fail "acd-inbox in PATH is $on_path, not the package: run $inbox_bin install"
	else
		pass "acd-inbox in PATH is the installed binary or not present"
	fi
	plugin_dir=${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/$id
	if [ -d "$plugin_dir" ]; then
		stale=0
		for f in manifest.json BarWidget.qml Panel.qml Service.qml model.mjs; do
			cmp -s "$plugin_dir/$f" "$ref/$f" || { fail "plugin file differs from the package: $f (run $inbox_bin install, then omarchy-restart-shell)"; stale=1; }
		done
		[ "$stale" -eq 0 ] && pass "installed plugin files match the package"
	else
		skip "plugin not installed for this user"
	fi
	if [ "$live" -eq 1 ]; then
		# Health is asserted, not inferred from the exit status of `status`.
		check "acd-inbox.service is active" systemctl --user is-active --quiet acd-inbox.service
		if status_out=$("$inbox_bin" status 2>&1); then
			case "$status_out" in *"service: running"*) pass "acd-inbox status: service running" ;; *) fail "acd-inbox status: service not running" ;; esac
			if grep -q '^socket: .* answers' <<<"$status_out"; then pass "acd-inbox status: socket answers"; else fail "acd-inbox status: socket does not answer"; fi
		else
			fail "acd-inbox status failed"
		fi
	fi
else
	skip "acd-inbox not installed at $inbox_bin (package agent-caller-omarchy)"
fi

if [ ! -x "$acd_bin" ] && [ ! -x "$inbox_bin" ]; then
	fail "neither package is installed under $prefix"
fi

if [ "$fails" -eq 0 ]; then
	echo "smoke test passed"
	exit 0
fi
echo "smoke test: $fails check(s) failed" >&2
exit 1
