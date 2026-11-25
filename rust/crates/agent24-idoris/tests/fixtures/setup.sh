# Assert the real CLI contract and credential boundary before simulating setup.
test "$1" = setup && test "$2" = omlx
out=''
key_flag=0
bind_count=0
endpoint=
auto=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --out-dir) shift; out="$1" ;;
        --api-key-stdin) key_flag=1 ;;
        --load-mode) exit 96 ;;
        --endpoint) shift; endpoint="$1" ;;
        --auto) auto=1 ;;
        --bind) shift; test "$1" = daily=real-model; bind_count=$((bind_count+1)) ;;
        *stdin-secret*) exit 90 ;;
    esac
    shift
done
# The shell adds PWD, SHLVL and _; setup inherits HOME and PATH for discovery.
if /usr/bin/env | /usr/bin/grep -vE '^(PATH=|HOME=|IDORIS_ALLOCATED_BUDGET_BYTES=0|PWD=|SHLVL=|_=)' | /usr/bin/grep -q .; then exit 93; fi
if /usr/bin/env | /usr/bin/grep -q 'stdin-secret\|IDORIS_OMLX_API_KEY\|PROXY'; then exit 91; fi
test "$IDORIS_ALLOCATED_BUDGET_BYTES" = 0
if [ "$auto" = 1 ]; then
    test "$key_flag" = 0 && test -z "$endpoint"
    if IFS= read -r key; then exit 95; fi
    printf '%s\n' '{"status":"error","code":"auth_required","endpoint":"http://127.0.0.1:8088"}'
    exit 1
fi
test "$endpoint" = http://127.0.0.1:8088
test "$key_flag" = 1 && test "$bind_count" = 1
IFS= read -r key
test "$key" = stdin-secret
if IFS= read -r extra; then exit 92; fi
