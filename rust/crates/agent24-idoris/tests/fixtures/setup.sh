# Assert the real CLI contract and credential boundary before simulating setup.
test "$1" = setup && test "$2" = omlx
out=''
key_flag=0
bind_count=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out-dir) shift; out="$1" ;;
        --api-key-stdin) key_flag=1 ;;
        --bind) shift; test "$1" = daily=real-model; bind_count=$((bind_count+1)) ;;
        *stdin-secret*) exit 90 ;;
    esac
    shift
done
test "$key_flag" = 1 && test "$bind_count" = 1
# The shell itself adds PWD, SHLVL and _; everything else is explicit.
if /usr/bin/env | /usr/bin/grep -vE '^(PATH=/usr/bin:/bin|IDORIS_ALLOCATED_BUDGET_BYTES=0|PWD=|SHLVL=|_=)' | /usr/bin/grep -q .; then exit 93; fi
if /usr/bin/env | /usr/bin/grep -q 'stdin-secret\|IDORIS_OMLX_API_KEY\|HOME=\|PROXY'; then exit 91; fi
test "$IDORIS_ALLOCATED_BUDGET_BYTES" = 0
IFS= read -r key
test "$key" = stdin-secret
if IFS= read -r extra; then exit 92; fi
