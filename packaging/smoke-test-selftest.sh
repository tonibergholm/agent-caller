#!/usr/bin/env bash
# Tests packaging/smoke-test.sh itself, with stub binaries in a fake prefix
# and stub systemctl and systemd-analyze in PATH. No systemd, daemon or
# network is needed. It runs the real script, not a copy.
#
#   packaging/smoke-test.sh runs as a normal user, so as root this re-runs
#   itself as `nobody` (runuser).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
if [ "$(id -u)" -eq 0 ]; then
	t=$(mktemp -d)
	trap 'rm -rf "$t"' EXIT
	chmod 1777 "$t"
	cp "$here/smoke-test.sh" "$here/smoke-test-selftest.sh" "$t/"
	chmod 755 "$t"/*.sh
	exec runuser -u nobody -- env "TMPDIR=$t" bash "$t/smoke-test-selftest.sh"
fi
smoke=$here/smoke-test.sh

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
prefix=$work/usr
stubs=$work/stubs
home=$work/home
mkdir -p "$prefix/bin" "$prefix/lib/systemd/user" "$prefix/share/licenses/agent-caller-omarchy" "$stubs" "$home"
id=tonibergholm.agentcaller
ref=$prefix/share/agent-caller/omarchy/$id
mkdir -p "$ref"
for f in manifest.json BarWidget.qml Panel.qml Service.qml model.mjs; do echo "$f" >"$ref/$f"; done
echo unit >"$prefix/lib/systemd/user/acd-inbox.service"
echo licence >"$prefix/share/licenses/agent-caller-omarchy/LICENSE"

# The fake acd-inbox: --version, and status output chosen by FAKE_STATUS.
cat >"$prefix/bin/acd-inbox" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
--version) echo "acd-inbox 0.9.0" ;;
status)
	case "${FAKE_STATUS:-healthy}" in
	healthy) echo "service: running"; echo "socket: /run/x answers (protocol 1, service acd-inbox)" ;;
	stopped) echo "service: not running"; echo "socket: /run/x does not answer" ;;
	nosocket) echo "service: running"; echo "socket: /run/x does not answer" ;;
	esac
	exit 0 # like the real command: success whatever the state
	;;
esac
STUB
chmod +x "$prefix/bin/acd-inbox"
# systemctl --user is-active: succeeds only when FAKE_ACTIVE=1.
cat >"$stubs/systemctl" <<'STUB'
#!/usr/bin/env bash
[ "${FAKE_ACTIVE:-1}" = 1 ]
STUB
printf '#!/usr/bin/env bash\nexit 0\n' >"$stubs/systemd-analyze"
# The fake is what a bare `acd-inbox` resolves to, even where a real one is installed.
ln -s "$prefix/bin/acd-inbox" "$stubs/acd-inbox"
chmod +x "$stubs/systemctl" "$stubs/systemd-analyze"

fails=0
run() { # description, expected (pass|fail), expected text in output, env... -- args
	local d=$1 want=$2 text=$3
	shift 3
	local out rc=0
	out=$(env "HOME=$home" "PATH=$stubs:$PATH" "$@" 2>&1) || rc=$?
	if { [ "$want" = pass ] && [ "$rc" -eq 0 ]; } || { [ "$want" = fail ] && [ "$rc" -eq 1 ]; }; then
		if [ -z "$text" ] || grep -qF -- "$text" <<<"$out"; then
			printf 'ok    %s\n' "$d"
			return 0
		fi
		printf 'FAIL  %s: output lacks: %s\n' "$d" "$text"
	else
		printf 'FAIL  %s: exit %s, wanted %s\n' "$d" "$rc" "$want"
	fi
	printf '%s\n' "$out" | sed 's/^/      /'
	fails=$((fails + 1))
}

run "healthy service passes --live" pass "acd-inbox.service is active" \
	FAKE_ACTIVE=1 FAKE_STATUS=healthy bash "$smoke" --live --prefix "$prefix"
run "stopped service fails --live (status itself exits 0)" fail "acd-inbox.service is active" \
	FAKE_ACTIVE=0 FAKE_STATUS=stopped bash "$smoke" --live --prefix "$prefix"
run "stopped service is also caught by the status text" fail "acd-inbox status: service not running" \
	FAKE_ACTIVE=1 FAKE_STATUS=stopped bash "$smoke" --live --prefix "$prefix"
run "active service with no answering socket fails --live" fail "acd-inbox status: socket does not answer" \
	FAKE_ACTIVE=1 FAKE_STATUS=nosocket bash "$smoke" --live --prefix "$prefix"
run "without --live a stopped service is not checked" pass "" \
	FAKE_ACTIVE=0 FAKE_STATUS=stopped bash "$smoke" --prefix "$prefix"

# A stale acd-inbox first in PATH (like ~/.local/bin) must fail and the
# message must name the packaged binary.
mkdir -p "$work/old"
printf '#!/usr/bin/env bash\necho old\n' >"$work/old/acd-inbox"
chmod +x "$work/old/acd-inbox"
run "a stale acd-inbox first in PATH fails and names the packaged binary" fail "run $prefix/bin/acd-inbox install" \
	"PATH=$work/old:$stubs:$PATH" bash "$smoke" --prefix "$prefix"
mkdir -p "$home/.local/bin"
cp "$work/old/acd-inbox" "$home/.local/bin/acd-inbox"
run "a stale per-user copy fails and names the packaged binary" fail "stale: run $prefix/bin/acd-inbox install" \
	bash "$smoke" --prefix "$prefix"

if [ "$fails" -eq 0 ]; then echo "smoke-test selftest passed"; exit 0; fi
echo "smoke-test selftest: $fails case(s) failed" >&2
exit 1
